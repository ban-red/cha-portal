//! Our PulseAudio-protocol server, what apps play into through libpulse. One
//! sink, `cha`; playback streams only (no sources, sample cache or modules
//! yet); samples come over the socket (no shared memory). The wire format is
//! the `pulseaudio` crate's; the server and its flow control are ours.
//!
//! Flow control follows PulseAudio's: a stream asks for at most `tlength`
//! bytes ahead of the mixer (REQUEST), starts once `prebuf` bytes are queued
//! (STARTED) and stops again when it runs dry (UNDERFLOW). Read and write
//! indexes are kept in the stream's bytes, so libpulse's latency and timing
//! queries are exact.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{CStr, CString};
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use pulseaudio::protocol::sample_spec::{MAX_CHANNELS, MAX_RATE};
use pulseaudio::protocol::stream::BufferAttr;
use pulseaudio::protocol::{
    self as pa, AuthReply, ChannelMap, ChannelVolume, ClientInfo, Command,
    CreatePlaybackStreamReply, FormatEncoding, FormatInfo, LookupReply, PlaybackLatency, Prop,
    Props, ProtocolError, PulseError, Request, SampleFormat, SampleSpec, ServerInfo,
    SetClientNameReply, SetPlaybackStreamBufferAttrReply, SinkFlags, SinkInfo, SinkInputInfo,
    SinkState, StatInfo, SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType,
    SubscriptionMask, Underflow, Volume,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::RATE;
use super::convert::Converter;

const SINK_NAME: &CStr = c"cha";
/// The sink's latency as apps see it: one mixer tick.
const SINK_LATENCY_US: u64 = 10_000;
/// Buffering for streams that leave it to the server (PulseAudio's 2 s would
/// put that much delay on the stream).
const DEFAULT_TLENGTH_US: u64 = 60_000;
/// Less than two mixer ticks underruns.
const MIN_TLENGTH_US: u64 = 20_000;

/// Messages to one client, fully encoded.
type Outbox = mpsc::UnboundedSender<Vec<u8>>;

#[derive(Default)]
pub struct Sink {
    state: Mutex<State>,
}

struct State {
    clients: BTreeMap<u32, Client>,
    /// Playback streams ("sink inputs") by index.
    inputs: BTreeMap<u32, Input>,
    next_client: u32,
    next_input: u32,
    volume: ChannelVolume,
    muted: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            clients: BTreeMap::new(),
            inputs: BTreeMap::new(),
            next_client: 0,
            next_input: 0,
            volume: ChannelVolume::norm(2),
            muted: false,
        }
    }
}

struct Client {
    out: Outbox,
    version: u16,
    props: Props,
    subscriptions: SubscriptionMask,
}

struct Input {
    client: u32,
    channel: u32,
    out: Outbox,
    version: u16,
    spec: SampleSpec,
    map: ChannelMap,
    attr: BufferAttr,
    props: Props,
    volume: ChannelVolume,
    muted: bool,
    corked: bool,
    /// False while prebuffering.
    playing: bool,
    underrunning: bool,
    queue: VecDeque<u8>,
    converter: Converter,
    read_index: i64,
    /// Bytes asked for and not yet written.
    requested: u64,
    /// A DRAIN to acknowledge once the queue runs out.
    drain: Option<u32>,
    underrun_for: u64,
    playing_for: u64,
}

impl Sink {
    /// Mixes one tick of every playing stream into `out` (interleaved stereo).
    pub fn mix(&self, out: &mut [f32]) {
        let mut state = self.state.lock().expect("sink lock");
        let master = if state.muted {
            [0.0; 2]
        } else {
            gains(&state.volume)
        };
        for input in state.inputs.values_mut() {
            input.mix(out, master);
        }
    }
}

impl Input {
    fn frame_bytes(&self) -> usize {
        self.spec.format.bytes_per_sample() * usize::from(self.spec.channels)
    }

    fn write_index(&self) -> i64 {
        self.read_index + self.queue.len() as i64
    }

    fn send(&self, command: &Command) {
        send(&self.out, command, self.version);
    }

    fn mix(&mut self, out: &mut [f32], master: [f32; 2]) {
        if !self.corked && self.playing {
            let gain = if self.muted {
                [0.0; 2]
            } else {
                let own = gains(&self.volume);
                [own[0] * master[0], own[1] * master[1]]
            };
            let (frames, bytes) = self.converter.mix(&mut self.queue, out, gain);
            self.read_index += bytes as i64;
            if frames == out.len() / 2 {
                self.playing_for += bytes as u64;
                self.underrun_for = 0;
                self.underrunning = false;
            } else {
                // Ran dry.
                let short = (out.len() / 2 - frames) as u64 * u64::from(self.spec.sample_rate)
                    / u64::from(RATE)
                    * self.frame_bytes() as u64;
                self.underrun_for += short;
                self.playing_for = 0;
                if let Some(seq) = self.drain.take() {
                    send_raw(&self.out, ack(seq));
                } else if !self.underrunning {
                    self.underrunning = true;
                    self.send(&Command::Underflow(Underflow {
                        channel: self.channel,
                        offset: self.read_index,
                    }));
                }
                if self.attr.pre_buffering > 0 {
                    self.playing = false;
                }
            }
        }
        self.request_more();
    }

    /// Asks for what the stream lacks of `tlength`, once that's at least
    /// `minreq`.
    fn request_more(&mut self) {
        let frame = self.frame_bytes().max(1) as u64;
        let target = u64::from(self.attr.target_length);
        let mut missing = target.saturating_sub(self.queue.len() as u64 + self.requested);
        missing -= missing % frame;
        if missing == 0 || missing < u64::from(self.attr.minimum_request_length) {
            return;
        }
        self.requested += missing;
        self.send(&Command::Request(Request {
            channel: self.channel,
            length: missing as u32,
        }));
    }

    /// Takes a memblock at `offset` from where `seek` says.
    fn write(&mut self, offset: i64, seek: u32, data: &[u8]) {
        // PA_SEEK_RELATIVE (0) and _END (3) are from the write index,
        // _ABSOLUTE (1) from the start, _RELATIVE_ON_READ (2) from the read index.
        let target = match seek {
            1 => offset,
            2 => self.read_index + offset,
            _ => self.write_index() + offset,
        };
        let delta = target - self.write_index();
        if delta > 0 {
            let room = (self.attr.max_length as usize).saturating_sub(self.queue.len());
            self.queue
                .extend(std::iter::repeat_n(0u8, (delta as usize).min(room)));
        } else if delta < 0 {
            let cut = (delta.unsigned_abs() as usize).min(self.queue.len());
            self.queue.truncate(self.queue.len() - cut);
        }
        let room = (self.attr.max_length as usize).saturating_sub(self.queue.len());
        let take = data.len().min(room);
        if take < data.len() {
            self.send(&Command::Overflow(self.channel));
        }
        self.queue.extend(&data[..take]);
        self.requested = self.requested.saturating_sub(data.len() as u64);
        if !self.playing && self.queue.len() >= self.attr.pre_buffering as usize {
            self.start();
        }
    }

    fn start(&mut self) {
        self.playing = true;
        self.underrunning = false;
        if self.version >= 13 {
            self.send(&Command::Started(self.channel));
        }
    }

    fn info(&self, index: u32) -> SinkInputInfo {
        SinkInputInfo {
            index,
            name: prop_string(&self.props, Prop::MediaName).unwrap_or_else(|| c"Playback".into()),
            owner_module_index: None,
            client_index: Some(self.client),
            sink_index: 0,
            sample_spec: self.spec,
            channel_map: self.map,
            cvolume: self.volume,
            buffer_latency: bytes_to_us(self.queue.len() as u64, &self.spec),
            sink_latency: SINK_LATENCY_US,
            resample_method: (self.spec.sample_rate != RATE).then(|| c"cha-sinc".into()),
            driver: Some(c"cha-streamer".into()),
            props: self.props.clone(),
            muted: self.muted,
            corked: self.corked,
            has_volume: true,
            volume_writable: true,
            format: FormatInfo::new(FormatEncoding::Pcm),
        }
    }
}

/// Left and right linear gains for a (cubic-scale) PulseAudio volume.
fn gains(volume: &ChannelVolume) -> [f32; 2] {
    let v: Vec<f32> = volume.channels().iter().map(Volume::to_linear).collect();
    match v.as_slice() {
        [] => [1.0; 2],
        [m] => [*m; 2],
        [l, r] => [*l, *r],
        all => {
            let mean = all.iter().sum::<f32>() / all.len() as f32;
            [mean; 2]
        }
    }
}

fn bytes_to_us(bytes: u64, spec: &SampleSpec) -> u64 {
    let frame = (spec.format.bytes_per_sample() * usize::from(spec.channels)).max(1) as u64;
    bytes / frame * 1_000_000 / u64::from(spec.sample_rate.max(1))
}

fn us_to_bytes(us: u64, spec: &SampleSpec) -> u64 {
    let frame = (spec.format.bytes_per_sample() * usize::from(spec.channels)) as u64;
    us * u64::from(spec.sample_rate) / 1_000_000 * frame
}

/// The server's choice for each attribute a client leaves at -1, and limits
/// on the rest; whole frames throughout (PulseAudio's fix_playback_buffer_attr).
fn fix_attr(asked: BufferAttr, spec: &SampleSpec) -> BufferAttr {
    let frame = (spec.format.bytes_per_sample() * usize::from(spec.channels)) as u32;
    let align = |v: u32| (v / frame * frame).max(frame);
    let max_length =
        if asked.max_length == u32::MAX || asked.max_length as usize > pa::MAX_MEMBLOCKQ_LENGTH {
            pa::MAX_MEMBLOCKQ_LENGTH as u32
        } else {
            asked.max_length
        };
    let max_length = align(max_length);
    let min_tlength = us_to_bytes(MIN_TLENGTH_US, spec) as u32;
    let target_length = if asked.target_length == u32::MAX {
        us_to_bytes(DEFAULT_TLENGTH_US, spec) as u32
    } else {
        asked.target_length.max(min_tlength)
    };
    let target_length = align(target_length.min(max_length));
    let minimum_request_length = if asked.minimum_request_length == u32::MAX {
        target_length / 4
    } else {
        asked.minimum_request_length
    };
    let minimum_request_length = align(minimum_request_length.min(target_length / 2));
    let max_prebuf = target_length + frame - minimum_request_length;
    let pre_buffering = if asked.pre_buffering == u32::MAX || asked.pre_buffering > max_prebuf {
        max_prebuf
    } else {
        asked.pre_buffering
    };
    BufferAttr {
        max_length,
        target_length,
        pre_buffering: pre_buffering / frame * frame,
        minimum_request_length,
        fragment_size: 0,
    }
}

fn prop_string(props: &Props, prop: Prop) -> Option<CString> {
    let bytes = props.get(prop)?;
    CStr::from_bytes_until_nul(bytes).ok().map(CStr::to_owned)
}

fn encode(f: impl FnOnce(&mut Vec<u8>) -> Result<usize, ProtocolError>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Err(err) = f(&mut buf) {
        warn!("audio: encoding a message: {err}");
        buf.clear();
    }
    buf
}

fn ack(seq: u32) -> Vec<u8> {
    encode(|b| pa::encode_ack_message(seq, b))
}

fn reply<R: pa::CommandReply>(seq: u32, r: &R, version: u16) -> Vec<u8> {
    encode(|b| pa::encode_reply_message(b, seq, r, version))
}

fn error(seq: u32, err: PulseError) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = pa::write_error(&mut buf, seq, &err);
    buf
}

fn send(out: &Outbox, command: &Command, version: u16) {
    send_raw(
        out,
        encode(|b| pa::encode_command_message(b, u32::MAX, command, version)),
    );
}

fn send_raw(out: &Outbox, message: Vec<u8>) {
    if !message.is_empty() {
        let _ = out.send(message);
    }
}

impl State {
    fn notify(
        &self,
        facility: SubscriptionEventFacility,
        event_type: SubscriptionEventType,
        index: u32,
    ) {
        let mask = match facility {
            SubscriptionEventFacility::Sink => SubscriptionMask::SINK,
            SubscriptionEventFacility::SinkInput => SubscriptionMask::SINK_INPUT,
            SubscriptionEventFacility::Client => SubscriptionMask::CLIENT,
            SubscriptionEventFacility::Server => SubscriptionMask::SERVER,
            _ => return,
        };
        let event = Command::SubscribeEvent(SubscriptionEvent {
            event_facility: facility,
            event_type,
            index: Some(index),
        });
        for client in self.clients.values() {
            if client.subscriptions.contains(mask) {
                send(&client.out, &event, client.version);
            }
        }
    }

    fn sink_info(&self) -> SinkInfo {
        let mut props = Props::new();
        props.set(Prop::DeviceDescription, c"Cha stream");
        props.set(Prop::DeviceClass, c"sound");
        let playing = self.inputs.values().any(|i| i.playing && !i.corked);
        SinkInfo {
            index: 0,
            name: SINK_NAME.into(),
            description: Some(c"Cha stream".into()),
            props,
            state: if playing {
                SinkState::Running
            } else {
                SinkState::Idle
            },
            sample_spec: mixer_spec(),
            channel_map: ChannelMap::stereo(),
            cvolume: self.volume,
            muted: self.muted,
            flags: SinkFlags::LATENCY | SinkFlags::DECIBEL_VOLUME,
            actual_latency: SINK_LATENCY_US,
            configured_latency: SINK_LATENCY_US,
            driver: Some(c"cha-streamer".into()),
            base_volume: Volume::NORM,
            volume_steps: Some(Volume::NORM.as_u32() + 1),
            ..SinkInfo::new_dummy(0)
        }
    }

    fn client_info(&self, index: u32) -> Option<ClientInfo> {
        let client = self.clients.get(&index)?;
        Some(ClientInfo {
            index,
            name: prop_string(&client.props, Prop::ApplicationName)
                .unwrap_or_else(|| c"Unknown".into()),
            owner_module_index: None,
            driver: Some(c"cha-streamer".into()),
            props: client.props.clone(),
        })
    }
}

/// The sink input behind one of a connection's channels.
fn input<'a>(
    channels: &HashMap<u32, u32>,
    inputs: &'a mut BTreeMap<u32, Input>,
    channel: u32,
) -> Option<(u32, &'a mut Input)> {
    let index = *channels.get(&channel)?;
    inputs.get_mut(&index).map(|i| (index, i))
}

fn mixer_spec() -> SampleSpec {
    SampleSpec {
        format: SampleFormat::Float32Le,
        channels: 2,
        sample_rate: RATE,
    }
}

fn is_our_sink(index: Option<u32>, name: Option<&CStr>) -> bool {
    match (index, name) {
        (Some(i), _) => i == 0,
        (None, Some(n)) => n == SINK_NAME || n == pa::DEFAULT_SINK,
        (None, None) => true,
    }
}

/// Binds the socket and hands it, and its directory, to the app's user.
pub fn bind(path: &Path, app_uid: Option<u32>) -> Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().context("socket path has no directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let _ = std::fs::remove_file(path);
    let listener = std::os::unix::net::UnixListener::bind(path)
        .with_context(|| format!("binding {}", path.display()))?;
    listener.set_nonblocking(true)?;
    if let Some(uid) = app_uid {
        std::os::unix::fs::chown(dir, Some(uid), Some(uid))?;
        std::os::unix::fs::chown(path, Some(uid), Some(uid))?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(listener)
}

pub async fn serve(sink: Arc<Sink>, listener: std::os::unix::net::UnixListener) {
    let listener = match UnixListener::from_std(listener) {
        Ok(l) => l,
        Err(err) => {
            warn!("audio: socket: {err}");
            return;
        }
    };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(Arc::clone(&sink), stream));
            }
            Err(err) => {
                warn!("audio: accept: {err}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn connection(sink: Arc<Sink>, stream: UnixStream) {
    let (mut rd, mut wr) = stream.into_split();
    let (out, mut outbox) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(message) = outbox.recv().await {
            if wr.write_all(&message).await.is_err() {
                break;
            }
        }
    });
    let id = {
        let mut state = sink.state.lock().expect("sink lock");
        let id = state.next_client;
        state.next_client += 1;
        state.clients.insert(
            id,
            Client {
                out: out.clone(),
                version: pa::MAX_VERSION,
                props: Props::new(),
                subscriptions: SubscriptionMask::empty(),
            },
        );
        state.notify(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::New,
            id,
        );
        id
    };
    let mut conn = Connection {
        sink: Arc::clone(&sink),
        id,
        out,
        version: pa::MAX_VERSION,
        authenticated: false,
        channels: HashMap::new(),
        next_channel: 0,
    };
    if let Err(err) = conn.run(&mut rd).await {
        debug!(client = id, "audio: connection closed: {err:#}");
    }
    conn.close();
    writer.abort();
}

struct Connection {
    sink: Arc<Sink>,
    id: u32,
    out: Outbox,
    version: u16,
    authenticated: bool,
    /// This connection's playback channels → sink input indexes.
    channels: HashMap<u32, u32>,
    next_channel: u32,
}

impl Connection {
    async fn run(&mut self, rd: &mut OwnedReadHalf) -> Result<()> {
        let mut header = [0u8; pa::DESCRIPTOR_SIZE];
        loop {
            match rd.read_exact(&mut header).await {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(err) => return Err(err.into()),
            }
            // Parsed by hand: the crate drops the seek mode from the flags.
            let word = |i: usize| u32::from_be_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
            let (length, channel) = (word(0) as usize, word(1));
            let offset = (u64::from(word(2)) << 32 | u64::from(word(3))) as i64;
            let flags = word(4);
            if length > pa::MAX_MEMBLOCKQ_LENGTH {
                bail!("a {length}-byte message");
            }
            let mut payload = vec![0u8; length];
            rd.read_exact(&mut payload).await?;
            if channel == u32::MAX {
                self.command(&payload)?;
            } else if flags & 0xc000_0000 == 0 {
                self.memblock(channel, offset, flags & 0xff, &payload);
            }
        }
    }

    fn close(&mut self) {
        let mut state = self.sink.state.lock().expect("sink lock");
        for index in self.channels.values() {
            if state.inputs.remove(index).is_some() {
                state.notify(
                    SubscriptionEventFacility::SinkInput,
                    SubscriptionEventType::Removed,
                    *index,
                );
            }
        }
        state.clients.remove(&self.id);
        state.notify(
            SubscriptionEventFacility::Client,
            SubscriptionEventType::Removed,
            self.id,
        );
    }

    fn memblock(&mut self, channel: u32, offset: i64, seek: u32, data: &[u8]) {
        let Some(index) = self.channels.get(&channel) else {
            return;
        };
        let mut state = self.sink.state.lock().expect("sink lock");
        if let Some(input) = state.inputs.get_mut(index) {
            input.write(offset, seek, data);
        }
    }

    fn send(&self, message: Vec<u8>) {
        send_raw(&self.out, message);
    }

    fn command(&mut self, payload: &[u8]) -> Result<()> {
        let (seq, command) =
            match Command::read_tag_prefixed(&mut Cursor::new(payload), self.version) {
                Ok(v) => v,
                Err(ProtocolError::Unimplemented(seq, tag)) => {
                    debug!(?tag, "audio: unimplemented command");
                    self.send(error(seq, PulseError::NotImplemented));
                    return Ok(());
                }
                Err(err) => {
                    // A tagstruct starts with the command and the sequence number,
                    // each a tagged u32: answer that one with an error.
                    if payload.len() >= 10 && payload[5] == b'L' {
                        let seq = u32::from_be_bytes(payload[6..10].try_into().unwrap());
                        let tag = u32::from_be_bytes(payload[1..5].try_into().unwrap());
                        warn!(tag, "audio: unreadable command: {err}");
                        self.send(error(seq, PulseError::Protocol));
                        return Ok(());
                    }
                    bail!("unreadable command: {err}");
                }
            };
        if !self.authenticated {
            let Command::Auth(auth) = command else {
                self.send(error(seq, PulseError::AccessDenied));
                return Ok(());
            };
            // The socket is the app's alone; the cookie isn't checked.
            if auth.version < pa::MIN_VERSION {
                self.send(error(seq, PulseError::Version));
                bail!("protocol version {} is too old", auth.version);
            }
            self.version = auth.version.min(pa::MAX_VERSION);
            self.authenticated = true;
            let mut state = self.sink.state.lock().expect("sink lock");
            if let Some(client) = state.clients.get_mut(&self.id) {
                client.version = self.version;
            }
            let r = AuthReply {
                version: pa::MAX_VERSION,
                use_memfd: false,
                use_shm: false,
            };
            self.send(reply(seq, &r, self.version));
            return Ok(());
        }
        self.handle(seq, command);
        Ok(())
    }

    fn handle(&mut self, seq: u32, command: Command) {
        use SubscriptionEventFacility as F;
        use SubscriptionEventType as T;
        let v = self.version;
        let sink = Arc::clone(&self.sink);
        let mut state = sink.state.lock().expect("sink lock");
        let state = &mut *state;
        let message = match command {
            Command::SetClientName(props) => {
                if let Some(client) = state.clients.get_mut(&self.id) {
                    let name = prop_string(&props, Prop::ApplicationName);
                    info!(client = self.id, name = ?name, "audio: client");
                    client.props = props;
                }
                reply(seq, &SetClientNameReply { client_id: self.id }, v)
            }
            Command::UpdateClientProplist(params) => {
                if let Some(client) = state.clients.get_mut(&self.id) {
                    for (k, val) in params.props.iter() {
                        client.props.set_bytes(&**k, &**val);
                    }
                }
                state.notify(F::Client, T::Changed, self.id);
                ack(seq)
            }
            Command::RemoveClientProplist => ack(seq),
            Command::Subscribe(mask) => {
                if let Some(client) = state.clients.get_mut(&self.id) {
                    client.subscriptions = mask;
                }
                ack(seq)
            }
            Command::GetServerInfo => {
                let info = ServerInfo {
                    server_name: Some(c"cha-streamer".into()),
                    server_version: Some(c"17.0".into()),
                    user_name: Some(c"cha".into()),
                    host_name: Some(c"cha".into()),
                    sample_spec: mixer_spec(),
                    channel_map: ChannelMap::stereo(),
                    default_sink_name: Some(SINK_NAME.into()),
                    default_source_name: None,
                    cookie: 0x0c4a_0001,
                };
                reply(seq, &info, v)
            }
            Command::Stat => reply(seq, &StatInfo::default(), v),

            Command::GetSinkInfoList => reply(seq, &vec![state.sink_info()], v),
            Command::GetSinkInfo(q) => {
                if is_our_sink(q.index, q.name.as_deref()) {
                    reply(seq, &state.sink_info(), v)
                } else {
                    error(seq, PulseError::NoEntity)
                }
            }
            Command::LookupSink(name) => {
                if is_our_sink(None, Some(name.as_c_str())) {
                    reply(seq, &LookupReply(0), v)
                } else {
                    error(seq, PulseError::NoEntity)
                }
            }
            Command::GetSinkInputInfoList => {
                let list: Vec<SinkInputInfo> = state
                    .inputs
                    .iter()
                    .map(|(i, input)| input.info(*i))
                    .collect();
                reply(seq, &list, v)
            }
            Command::GetSinkInputInfo(index) => match state.inputs.get(&index) {
                Some(input) => reply(seq, &input.info(index), v),
                None => error(seq, PulseError::NoEntity),
            },
            Command::GetClientInfoList => {
                let list: Vec<ClientInfo> = state
                    .clients
                    .keys()
                    .filter_map(|i| state.client_info(*i))
                    .collect();
                reply(seq, &list, v)
            }
            Command::GetClientInfo(index) => match state.client_info(index) {
                Some(info) => reply(seq, &info, v),
                None => error(seq, PulseError::NoEntity),
            },
            // Nothing of these exists (yet): empty lists, no entities.
            Command::GetSourceInfoList => reply(seq, &pa::SourceInfoList::new(), v),
            Command::GetSourceOutputInfoList => reply(seq, &pa::SourceOutputInfoList::new(), v),
            Command::GetModuleInfoList => reply(seq, &pa::ModuleInfoList::new(), v),
            Command::GetCardInfoList => reply(seq, &pa::CardInfoList::new(), v),
            Command::GetSampleInfoList => reply(seq, &pa::SampleInfoList::new(), v),
            Command::GetSourceInfo(_)
            | Command::GetSourceOutputInfo(_)
            | Command::GetModuleInfo(_)
            | Command::GetCardInfo(_)
            | Command::GetSampleInfo(_)
            | Command::LookupSource(_)
            | Command::CreateRecordStream(_)
            | Command::PlaySample(_) => error(seq, PulseError::NoEntity),

            Command::CreatePlaybackStream(params) => {
                let spec = params.sample_spec;
                let channels = usize::from(spec.channels);
                if spec.format == SampleFormat::Invalid
                    || channels == 0
                    || channels > usize::from(MAX_CHANNELS)
                    || spec.sample_rate == 0
                    || spec.sample_rate > MAX_RATE
                {
                    error(seq, PulseError::Invalid)
                } else {
                    let map = if usize::from(params.channel_map.num_channels()) == channels {
                        params.channel_map
                    } else if channels == 2 {
                        ChannelMap::stereo()
                    } else {
                        ChannelMap::mono()
                    };
                    let attr = fix_attr(params.buffer_attr, &spec);
                    let volume = params
                        .cvolume
                        .filter(|c| c.channels().len() == channels)
                        .unwrap_or_else(|| ChannelVolume::norm(spec.channels));
                    let index = state.next_input;
                    state.next_input += 1;
                    let channel = self.next_channel;
                    self.next_channel += 1;
                    let by_index = params.flags.no_remap_channels;
                    let input = Input {
                        client: self.id,
                        channel,
                        out: self.out.clone(),
                        version: v,
                        spec,
                        map,
                        attr,
                        props: params.props,
                        volume,
                        muted: params.flags.start_muted.unwrap_or(false),
                        corked: params.flags.start_corked,
                        playing: attr.pre_buffering == 0,
                        underrunning: false,
                        queue: VecDeque::with_capacity(attr.target_length as usize * 2),
                        converter: Converter::new(&spec, &map, by_index),
                        read_index: 0,
                        requested: u64::from(attr.target_length),
                        drain: None,
                        underrun_for: 0,
                        playing_for: 0,
                    };
                    info!(
                        index,
                        name = ?prop_string(&input.props, Prop::MediaName),
                        format = ?spec.format,
                        rate = spec.sample_rate,
                        channels,
                        tlength_ms = bytes_to_us(attr.target_length.into(), &spec) / 1000,
                        prebuf_ms = bytes_to_us(attr.pre_buffering.into(), &spec) / 1000,
                        minreq_ms = bytes_to_us(attr.minimum_request_length.into(), &spec) / 1000,
                        "audio: new stream"
                    );
                    self.channels.insert(channel, index);
                    state.inputs.insert(index, input);
                    state.notify(F::SinkInput, T::New, index);
                    let r = CreatePlaybackStreamReply {
                        channel,
                        stream_index: index,
                        requested_bytes: attr.target_length,
                        buffer_attr: attr,
                        sample_spec: spec,
                        channel_map: map,
                        stream_latency: SINK_LATENCY_US,
                        sink_index: 0,
                        sink_name: Some(SINK_NAME.into()),
                        suspended: false,
                        format: FormatInfo::new(FormatEncoding::Pcm),
                    };
                    reply(seq, &r, v)
                }
            }
            Command::DeletePlaybackStream(channel) => match self.channels.remove(&channel) {
                Some(index) => {
                    state.inputs.remove(&index);
                    state.notify(F::SinkInput, T::Removed, index);
                    ack(seq)
                }
                None => error(seq, PulseError::NoEntity),
            },
            Command::CorkPlaybackStream(p) => {
                match input(&self.channels, &mut state.inputs, p.channel) {
                    Some((index, i)) => {
                        i.corked = p.cork;
                        state.notify(F::SinkInput, T::Changed, index);
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::FlushPlaybackStream(channel) => {
                match input(&self.channels, &mut state.inputs, channel) {
                    Some((_, i)) => {
                        // Dropped, not played: the read index stays and the write
                        // index falls back to it.
                        i.queue.clear();
                        if i.attr.pre_buffering > 0 {
                            i.playing = false;
                        }
                        i.request_more();
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::TriggerPlaybackStream(channel) => {
                match input(&self.channels, &mut state.inputs, channel) {
                    Some((_, i)) => {
                        if !i.playing {
                            i.start();
                        }
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::PrebufPlaybackStream(channel) => {
                match input(&self.channels, &mut state.inputs, channel) {
                    Some((_, i)) => {
                        if i.attr.pre_buffering > 0 {
                            i.playing = false;
                        }
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::DrainPlaybackStream(channel) => {
                match input(&self.channels, &mut state.inputs, channel) {
                    Some((_, i)) if i.queue.is_empty() => ack(seq),
                    Some((_, i)) => {
                        // Acknowledged by the mixer once played out; a stream
                        // still prebuffering plays what it has.
                        if !i.playing {
                            i.start();
                        }
                        i.drain = Some(seq);
                        Vec::new()
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::GetPlaybackLatency(p) => {
                match input(&self.channels, &mut state.inputs, p.channel) {
                    Some((_, i)) => {
                        let r = PlaybackLatency {
                            sink_usec: SINK_LATENCY_US,
                            source_usec: 0,
                            playing: i.playing && !i.corked,
                            local_time: p.now,
                            remote_time: SystemTime::now(),
                            write_offset: i.write_index(),
                            read_offset: i.read_index,
                            underrun_for: i.underrun_for,
                            playing_for: i.playing_for,
                        };
                        reply(seq, &r, v)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::SetPlaybackStreamBufferAttr(p) => {
                match input(&self.channels, &mut state.inputs, p.index) {
                    Some((_, i)) => {
                        i.attr = fix_attr(p.buffer_attr, &i.spec);
                        let r = SetPlaybackStreamBufferAttrReply {
                            buffer_attr: i.attr,
                            configured_sink_latency: SINK_LATENCY_US,
                        };
                        i.request_more();
                        reply(seq, &r, v)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::UpdatePlaybackStreamSampleRate(p) => {
                match input(&self.channels, &mut state.inputs, p.index) {
                    Some((_, i)) if p.sample_rate > 0 && p.sample_rate <= MAX_RATE => {
                        i.spec.sample_rate = p.sample_rate;
                        i.converter.set_rate(p.sample_rate);
                        ack(seq)
                    }
                    Some(_) => error(seq, PulseError::Invalid),
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::SetPlaybackStreamName(p) => {
                match input(&self.channels, &mut state.inputs, p.index) {
                    Some((index, i)) => {
                        i.props.set(Prop::MediaName, p.name.as_c_str());
                        state.notify(F::SinkInput, T::Changed, index);
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::UpdatePlaybackStreamProplist(p) => {
                match input(&self.channels, &mut state.inputs, p.index) {
                    Some((index, i)) => {
                        for (k, val) in p.props.iter() {
                            i.props.set_bytes(&**k, &**val);
                        }
                        state.notify(F::SinkInput, T::Changed, index);
                        ack(seq)
                    }
                    None => error(seq, PulseError::NoEntity),
                }
            }
            Command::RemovePlaybackStreamProplist(_) => ack(seq),

            Command::SetSinkInputVolume(p) => match state.inputs.get_mut(&p.index) {
                Some(i) => {
                    if p.volume.channels().len() == usize::from(i.spec.channels) {
                        i.volume = p.volume;
                    }
                    state.notify(F::SinkInput, T::Changed, p.index);
                    ack(seq)
                }
                None => error(seq, PulseError::NoEntity),
            },
            Command::SetSinkInputMute(p) => match state.inputs.get_mut(&p.index) {
                Some(i) => {
                    i.muted = p.mute;
                    state.notify(F::SinkInput, T::Changed, p.index);
                    ack(seq)
                }
                None => error(seq, PulseError::NoEntity),
            },
            Command::KillSinkInput(index) => match state.inputs.remove(&index) {
                Some(i) => {
                    i.send(&Command::PlaybackStreamKilled(i.channel));
                    state.notify(F::SinkInput, T::Removed, index);
                    ack(seq)
                }
                None => error(seq, PulseError::NoEntity),
            },
            Command::SetSinkVolume(p) if is_our_sink(p.device_index, p.device_name.as_deref()) => {
                state.volume = match p.volume.channels() {
                    [m] => {
                        let mut stereo = ChannelVolume::empty();
                        stereo.push(*m);
                        stereo.push(*m);
                        stereo
                    }
                    _ => p.volume,
                };
                state.notify(F::Sink, T::Changed, 0);
                ack(seq)
            }
            Command::SetSinkMute(p) if is_our_sink(p.device_index, p.device_name.as_deref()) => {
                state.muted = p.mute;
                state.notify(F::Sink, T::Changed, 0);
                ack(seq)
            }
            Command::SetSinkVolume(_) | Command::SetSinkMute(_) => error(seq, PulseError::NoEntity),
            // One sink, always on: these change nothing.
            Command::SetDefaultSink(_)
            | Command::SuspendSink(_)
            | Command::MoveSinkInput(_)
            | Command::SetDefaultSource(_) => ack(seq),
            Command::Extension(_) => error(seq, PulseError::NoExtension),
            Command::Reply | Command::Error(_) | Command::Timeout => Vec::new(),
            other => {
                debug!(tag = ?other.tag(), "audio: unsupported command");
                error(seq, PulseError::NotSupported)
            }
        };
        send_raw(&self.out, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s16_stereo() -> SampleSpec {
        SampleSpec {
            format: SampleFormat::S16Le,
            channels: 2,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn defaults_are_low_latency() {
        let unset = BufferAttr {
            max_length: u32::MAX,
            target_length: u32::MAX,
            pre_buffering: u32::MAX,
            minimum_request_length: u32::MAX,
            fragment_size: u32::MAX,
        };
        let a = fix_attr(unset, &s16_stereo());
        // 60 ms of 48 kHz s16 stereo, a quarter of it per request.
        assert_eq!(a.target_length, 11_520);
        assert_eq!(a.minimum_request_length, 2_880);
        assert_eq!(a.pre_buffering, 11_520 + 4 - 2_880);
        assert_eq!(a.max_length as usize, pa::MAX_MEMBLOCKQ_LENGTH);
    }

    #[test]
    fn tiny_buffers_get_two_ticks() {
        let asked = BufferAttr {
            max_length: u32::MAX,
            target_length: 400,
            pre_buffering: 0,
            minimum_request_length: 2,
            fragment_size: u32::MAX,
        };
        let a = fix_attr(asked, &s16_stereo());
        assert_eq!(a.target_length, 3_840);
        assert_eq!(a.pre_buffering, 0);
        assert_eq!(a.minimum_request_length, 4);
    }
}
