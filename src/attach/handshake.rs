//! Version negotiation (plan section 3.2): the host picks `min(client_max, host_max)` and refuses
//! when that is below `max(client_min, host_min)`. Pure and table-testable.

use super::frame::ATTACH_PROTO_VERSION;

pub const HOST_PROTO_MIN: u16 = 1;
pub const HOST_PROTO_MAX: u16 = ATTACH_PROTO_VERSION;

/// Why a `Hello` was refused; rendered into `Refuse.reason`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

/// Picks the protocol version for a client advertising `[client_min, client_max]`.
pub fn negotiate(client_min: u16, client_max: u16) -> Result<u16, Refusal> {
    if client_min == 0 || client_min > client_max {
        return Err(Refusal(format!("invalid protocol range {client_min}..{client_max}")));
    }
    let picked = client_max.min(HOST_PROTO_MAX);
    if picked < client_min.max(HOST_PROTO_MIN) {
        return Err(Refusal(format!(
            "no common attach protocol: client {client_min}..{client_max}, host {HOST_PROTO_MIN}..{HOST_PROTO_MAX}; upgrade or restart the host"
        )));
    }
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matrix() {
        let cases: &[(u16, u16, Option<u16>)] = &[
            (1, 1, Some(1)),
            (1, 9, Some(HOST_PROTO_MAX)),
            (0, 1, None),
            (2, 1, None),
            (HOST_PROTO_MAX + 1, HOST_PROTO_MAX + 5, None),
            (u16::MAX, u16::MAX, None),
        ];
        for &(lo, hi, want) in cases {
            assert_eq!(negotiate(lo, hi).ok(), want, "{lo}..{hi}");
        }
    }

    #[test]
    fn refusal_names_host_range() {
        let r = negotiate(99, 100).unwrap_err();
        assert!(r.0.contains(&format!("{HOST_PROTO_MIN}..{HOST_PROTO_MAX}")));
    }
}
