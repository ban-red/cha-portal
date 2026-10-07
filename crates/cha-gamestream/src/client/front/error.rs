use crate::front::identity::IdentityError;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("bad host address {0:?}")]
    Address(String),
    #[error("can't reach {addr}: {source}")]
    Connect {
        addr: String,
        source: std::io::Error,
    },
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("TLS: {0}")]
    Tls(String),
    #[error("HTTP: {0}")]
    Http(String),
    /// The host answered 401: it doesn't know our certificate.
    #[error("the host doesn't accept our certificate: this client isn't paired with it")]
    NotPaired,
    /// HTTPS needs the host's certificate, which pairing returns.
    #[error("the host's certificate isn't known: pair first")]
    NoPinnedCert,
    /// The `status_code` of nvhttp's XML, when it isn't 200 (busy, no such app, ...).
    #[error("the host answered {code}: {message}")]
    Host { code: i32, message: String },
    #[error("the host sent something unreadable: {0}")]
    Malformed(String),
    #[error("RTSP {step}: {detail}")]
    Rtsp {
        step: &'static str,
        /// The RTSP status, when the host answered one.
        status: Option<u16>,
        detail: String,
    },
    /// What we asked for isn't something the host offers.
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Pairing(#[from] PairingError),
    #[error(transparent)]
    Identity(#[from] IdentityError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    /// The host's answers prove it was given another PIN than ours.
    #[error("the PIN is wrong")]
    WrongPin,
    /// The host answered the first step without its certificate: another
    /// client is pairing.
    #[error("the host is already pairing with another client")]
    AlreadyInProgress,
    /// The host never approved: nobody gave it the PIN in time, or it was refused.
    #[error("the host didn't accept the pairing (no PIN given, or refused)")]
    Declined,
    #[error("the host refused pairing at step {stage}")]
    Refused { stage: u8 },
    /// The host's secret isn't signed by the certificate it gave us.
    #[error("the host's pairing secret doesn't verify against its certificate")]
    Mitm,
    #[error("{0}")]
    Unsupported(String),
}
