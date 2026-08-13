//! Monero miner HTTP dispatch: XMRig and/or SRBMiner-Multi on `MONERO_API_PORT`.
//!
//! One MinerPerf write per tick (`{coin}_miner_telemetry`). `auto` probes XMRig
//! `/1/summary` and SRB `GET /` in parallel and prefers XMRig when both succeed.

use super::{TelemetryRecord, srbminer, xmrig};

/// Which Monero miner HTTP adapter to use on `MONERO_API_PORT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum MoneroMinerKind {
    /// Probe XMRig `/1/summary` and SRBMiner `GET /` in parallel; prefer XMRig.
    #[default]
    Auto,
    /// XMRig only.
    Xmrig,
    /// SRBMiner-Multi only.
    Srbminer,
}

pub async fn poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
    kind: MoneroMinerKind,
) -> TelemetryRecord {
    match kind {
        MoneroMinerKind::Xmrig => xmrig::poll(client, endpoint, coin).await,
        MoneroMinerKind::Srbminer => srbminer::poll(client, endpoint, coin).await,
        MoneroMinerKind::Auto => {
            let (xmrig_rec, srb_rec) = tokio::join!(
                xmrig::poll(client, endpoint, coin),
                srbminer::poll(client, endpoint, coin),
            );
            if xmrig_rec.envelope.is_some() {
                xmrig_rec
            } else {
                srb_rec
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_auto() {
        assert_eq!(MoneroMinerKind::default(), MoneroMinerKind::Auto);
    }
}
