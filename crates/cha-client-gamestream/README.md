# cha-client-gamestream

The native client's GameStream transport ([ADR 0010](../../docs/adr/0010-native-client-macos-first.md)): `GameStream` implements `cha_client::Transport` on `cha-gamestream`'s client half ([ADR 0011](../../docs/adr/0011-own-gamestream-client.md)), so one client plays Sunshine and Apollo PCs and our own nodes ([ADR 0009](../../docs/adr/0009-gamestream-host-module.md)).

```rust
let player = GameStream::open(dir)?;            // browse the LAN; identity and hosts under `dir`
let host = player.add_host("192.168.1.20").await?;   // or wait for player.hosts()
let Pairing { pin, done } = player.pair(&host.id).await?;   // show `pin`; type it on the host
done.await?;
let apps = player.apps(&host.id).await?;
let session = player.launch(&host.id, apps[0].id, config).await?;
```

| Path | What |
|---|---|
| `src/lib.rs` | `GameStream`: the `Transport`, pairing, the refresh task |
| `src/hosts.rs` | the host list and its aging (found hosts drop after 2 min unseen; paired and added ones stay), address parsing |
| `src/discovery.rs` | the `_nvstream._tcp.local.` browse |
| `src/store.rs` | client identity (`client-cert.pem`, `client-key.pem`), each host's certificate (`hosts/<unique id>/server-cert.pem`) and `hosts.json` (files 0600); the layout installs paired with the earlier library already have |
| `src/stream.rs` | launch or resume, codec choice, the relay task, `SessionControl` |

## Behaviour and limits

- Every 30 s each known host is read with `/serverinfo` (over HTTPS with our certificate where paired, so `paired` is right). A host list persists across restarts, but there is no "offline" flag: a saved host that is down still shows, and launching fails with the connection error.
- A client's `uniqueid` is `cha-player` and the device is named `ChaPlayer`, as before. `pair` returns a random four-digit PIN at once; the pairing runs in the background (5 minutes) whether or not `done` is polled.
- The codec is the first of `StreamConfig::codecs` the host can encode (AV1, HEVC, H.264). Stereo audio only (anything else is an error). Video and audio are encrypted only when the host insists; the client can decrypt both, so the player could ask for it.
- A host running the requested app is resumed. A host running another app is refused with an error: it may be a game the user is in.
- Video is whole access units with the keyframe flag from the packet header. A frame the network lost beyond what FEC recovers is dropped inside the media client, which asks the host for a keyframe and resumes at it. If the player falls behind, frames are dropped the same way. Audio and feedback drop when the player's channel is full.
- `stop(true)` waits for the goodbye to reach the host, then cancels the app on the host. Dropping the `Session` stops without quitting.
- Rumble arrives as levels 0..1 and RGB LEDs as `Feedback::Led`; the gyro, trigger rumble and adaptive-trigger packets are ignored.

## Develop

```bash
cargo test -p cha-client-gamestream
```

`tests/loopback.rs` runs the whole transport against `cha-gamestream`'s own host with its fake directory and backend (the rig of that crate's tests, included by path), in process on loopback. What needs a real Sunshine or Apollo host: mDNS discovery on a LAN, pairing through their PIN page, HEVC and H.264 launches, resume of a running game, and quitting it.
