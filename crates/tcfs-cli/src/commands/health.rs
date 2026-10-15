//! `tcfs health`: a one-line, exit-coded health verdict for supervisors.
//!
//! A health check that restarts `tcfsd` must tell a wedged daemon apart from
//! a backend outage. Restarting the daemon cannot fix an unreachable
//! backend; doing it on every timer tick is a restart storm (the
//! 2026-10-15 estate check counted ~115 restarts a day while the backend was
//! paused). The verdicts and exit codes are:
//!
//! | Verdict | Exit | Meaning | Supervisor action |
//! | --- | --- | --- | --- |
//! | `UP` | 0 | daemon answers, storage reachable | none |
//! | `DAEMON_DOWN` | 2 | no socket, connection refused, or the status RPC failed | restart, with backoff |
//! | `BACKEND_DOWN` | 3 | daemon answers, storage unreachable | report DOWN, do not restart |
//! | `DAEMON_BUSY` | 4 | daemon accepted the connection but did not answer in time | report, do not restart |
//!
//! `DAEMON_BUSY` is kept apart from `DAEMON_DOWN` because a daemon that
//! accepts connections is running: a slow answer during a long transfer or a
//! backend stall is not a reason to kill it (and its transfer). A daemon that
//! stays busy across many checks needs an operator, not a restart loop.

/// What the status RPC did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusProbe {
    /// The daemon answered with its storage verdict.
    Answered { storage_ok: bool },
    /// The daemon accepted the connection but the RPC did not finish in time.
    TimedOut,
    /// The connection was refused or the RPC failed outright.
    Failed,
}

/// What a probe of the daemon saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthProbe {
    /// The daemon's Unix socket exists.
    pub socket_present: bool,
    /// The status RPC's outcome (ignored when the socket is missing).
    pub status: StatusProbe,
}

/// The verdict a supervisor acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonHealth {
    Up,
    BackendDown,
    DaemonDown,
    DaemonBusy,
}

impl DaemonHealth {
    pub fn classify(probe: HealthProbe) -> Self {
        if !probe.socket_present {
            return Self::DaemonDown;
        }
        match probe.status {
            StatusProbe::Answered { storage_ok: true } => Self::Up,
            StatusProbe::Answered { storage_ok: false } => Self::BackendDown,
            StatusProbe::TimedOut => Self::DaemonBusy,
            StatusProbe::Failed => Self::DaemonDown,
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::Up => 0,
            Self::DaemonDown => 2,
            Self::BackendDown => 3,
            Self::DaemonBusy => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::DaemonDown => "DAEMON_DOWN",
            Self::BackendDown => "BACKEND_DOWN",
            Self::DaemonBusy => "DAEMON_BUSY",
        }
    }

    /// Whether restarting `tcfsd` could plausibly fix this state.
    pub fn restart_may_help(self) -> bool {
        matches!(self, Self::DaemonDown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(socket_present: bool, status: StatusProbe) -> DaemonHealth {
        DaemonHealth::classify(HealthProbe {
            socket_present,
            status,
        })
    }

    #[test]
    fn backend_outage_is_down_but_never_a_restart_reason() {
        let verdict = probe(true, StatusProbe::Answered { storage_ok: false });
        assert_eq!(verdict, DaemonHealth::BackendDown);
        assert_eq!(verdict.label(), "BACKEND_DOWN");
        assert_eq!(verdict.exit_code(), 3);
        assert!(!verdict.restart_may_help());
    }

    #[test]
    fn socket_presence_alone_is_not_health() {
        // The old Darwin check said HEALTHY whenever the socket existed.
        let verdict = probe(true, StatusProbe::Failed);
        assert_eq!(verdict, DaemonHealth::DaemonDown);
        assert_eq!(verdict.exit_code(), 2);
        assert!(verdict.restart_may_help());
    }

    #[test]
    fn a_slow_answer_is_busy_not_down_and_never_a_restart_reason() {
        // A long push or a stalled backend can delay the status RPC; killing
        // the daemon would also kill the transfer.
        let verdict = probe(true, StatusProbe::TimedOut);
        assert_eq!(verdict, DaemonHealth::DaemonBusy);
        assert_eq!((verdict.label(), verdict.exit_code()), ("DAEMON_BUSY", 4));
        assert!(!verdict.restart_may_help());
    }

    #[test]
    fn missing_socket_is_daemon_down_and_healthy_is_up() {
        assert_eq!(probe(false, StatusProbe::Failed), DaemonHealth::DaemonDown);
        assert_eq!(
            probe(false, StatusProbe::TimedOut),
            DaemonHealth::DaemonDown
        );
        let up = probe(true, StatusProbe::Answered { storage_ok: true });
        assert_eq!(up, DaemonHealth::Up);
        assert_eq!((up.label(), up.exit_code()), ("UP", 0));
        assert!(!up.restart_may_help());
    }
}
