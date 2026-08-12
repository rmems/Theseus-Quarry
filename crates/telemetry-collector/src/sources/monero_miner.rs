//! Monero miner HTTP dispatch: XMRig and/or SRBMiner-Multi on `MONERO_API_PORT`.
//!
//! One MinerPerf write per tick (`{coin}_miner_telemetry`). `auto` sniffs XMRig
//! `/1/summary` first, then SRB `GET /`.

use super::{TelemetryRecord, srbminer, xmrig};

/// Which Monero miner HTTP adapter to use on `MONERO_API_PORT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum MoneroMinerKind {
    /// Try XMRig `/1/summary`, then SRBMiner `GET /`.
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
            let xmrig_rec = xmrig::poll(client, endpoint, coin).await;
            if xmrig_rec.envelope.is_some() {
                xmrig_rec
            } else {
                srbminer::poll(client, endpoint, coin).await
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
