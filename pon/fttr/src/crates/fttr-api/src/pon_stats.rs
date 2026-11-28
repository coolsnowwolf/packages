//! AN4475-G2 FSL PON hardware counter snapshot and modulo delta.
//! These are read-only PHY/GEM/RSM counters, not ONU FEC PM and not a
//! link-readiness signal. Counter widths and clear-on-read behavior are
//! hardware observations, not guaranteed semantics.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PonStatsRequest {
    pub onu_id: u8,
    pub serial: String,
    pub generation: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonUsGtc {
    pub det_ok_normal: u32,
    pub det_ok_ranging: u32,
    pub det_fail_normal: u32,
    pub det_fail_ranging: u32,
    pub fec_block: u32,
    pub fec_correct: u32,
    pub fec_correct_byte: u32,
    pub fec_uncorrect: u32,
    pub bip_err: u32,
    pub burst: u32,
    pub normal_ploam_crc_err: u32,
    pub dbru_crc_err: u32,
    pub normal_opt_rst: u32,
    pub lof_int: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonUsGem {
    pub omci_lf0: u32,
    pub omci_lf1: u32,
    pub eth_lf0: u32,
    pub eth_lf1: u32,
    pub omci_bytes: u32,
    pub eth_bytes: u32,
    pub idle_bytes: u32,
    pub hec_uncorrect: u32,
    pub hec_correct: u32,
    pub pli_err: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonDsGem {
    pub omci_lf0: u32,
    pub omci_lf1: u32,
    pub eth_lf0: u32,
    pub eth_lf1: u32,
    pub eth_bytes: u32,
    pub uc_pkt: u32,
    pub mc_pkt: u32,
    pub bc_pkt: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonOnuDet {
    pub us_bip_err: u32,
    pub us_burst: u32,
    pub us_det_fail: u32,
    pub us_det_drift: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonOnuUsRsm {
    pub omci_pkt: u32,
    pub uc_pkt: u32,
    pub eth_bytes: u32,
    pub mc_pkt: u32,
    pub bc_pkt: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonOnuDsPkt {
    pub omci_pkt: u32,
    pub eth_pkt: u32,
    pub uc_pkt: u32,
    pub mc_pkt: u32,
    pub bc_pkt: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonNniUs {
    pub eth_rx: u32,
    pub omci_rx: u32,
    pub eth_tx: u32,
    pub omci_tx: u32,
    pub eth_shm2adp: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonNniDs {
    pub eth_rx: u32,
    pub omci_rx: u32,
    pub err_rx: u32,
    pub eth_wr: u32,
    pub eth_tx: u32,
    pub switch_tx: u32,
    pub switch_rx: u32,
    pub novlan: u32,
    pub switch_fail: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PonEthMac {
    pub rx_code_err: u32,
    pub rx_fcs_ok: u32,
    pub rx_fcs_err: u32,
    pub rx_good: u32,
    pub rx_bad: u32,
    pub rx_uc: u32,
    pub rx_bc: u32,
    pub tx_good: u32,
    pub tx_uc: u32,
    pub tx_mc: u32,
    pub tx_bc: u32,
    pub tx_undersize: u32,
    pub tx_oversize: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PonStatsSnapshot {
    pub schema_version: u8,
    pub onu_index: u16,
    /// Unix milliseconds when the read started; used only for delta elapsed time.
    pub started_unix_ms: u64,
    pub us_gtc: PonUsGtc,
    pub us_gem: PonUsGem,
    pub ds_gem: PonDsGem,
    pub onu_det: PonOnuDet,
    pub onu_us_rsm: PonOnuUsRsm,
    pub onu_ds_pkt: PonOnuDsPkt,
    /// Raw multiperiod Ethernet size buckets (not normalized into deltas).
    pub rsm_eth_sizes: Vec<u32>,
    /// Raw RSM drop words (low three included in the flat delta map).
    pub rsm_drop_raw: Vec<u32>,
    pub nni_us: PonNniUs,
    pub nni_ds: PonNniDs,
    pub eth_mac: PonEthMac,
    /// Read-only PHY configuration registers; scalar or array values.
    pub phy_cfg: BTreeMap<String, serde_json::Value>,
}

impl PonStatsSnapshot {
    /// Flatten all 32-bit counters into stable keys for modulo comparison.
    /// PHY configuration and raw size buckets are deliberately excluded.
    pub fn flat_counters(&self) -> BTreeMap<String, u32> {
        let mut m = BTreeMap::new();
        // Keep wire keys stable: one entry per named 32-bit counter.
        macro_rules! flat {
            ($prefix:ident, $($field:ident),+ $(,)?) => {
                $(m.insert(format!("{}.{}", stringify!($prefix), stringify!($field)), self.$prefix.$field);)+
            };
        }
        flat!(
            us_gtc,
            det_ok_normal,
            det_ok_ranging,
            det_fail_normal,
            det_fail_ranging,
            fec_block,
            fec_correct,
            fec_correct_byte,
            fec_uncorrect,
            bip_err,
            burst,
            normal_ploam_crc_err,
            dbru_crc_err,
            normal_opt_rst,
            lof_int
        );
        flat!(
            us_gem,
            omci_lf0,
            omci_lf1,
            eth_lf0,
            eth_lf1,
            omci_bytes,
            eth_bytes,
            idle_bytes,
            hec_uncorrect,
            hec_correct,
            pli_err
        );
        flat!(ds_gem, omci_lf0, omci_lf1, eth_lf0, eth_lf1, eth_bytes, uc_pkt, mc_pkt, bc_pkt);
        flat!(onu_det, us_bip_err, us_burst, us_det_fail, us_det_drift);
        flat!(onu_us_rsm, omci_pkt, uc_pkt, eth_bytes, mc_pkt, bc_pkt);
        flat!(onu_ds_pkt, omci_pkt, eth_pkt, uc_pkt, mc_pkt, bc_pkt);
        flat!(nni_us, eth_rx, omci_rx, eth_tx, omci_tx, eth_shm2adp);
        flat!(
            nni_ds,
            eth_rx,
            omci_rx,
            err_rx,
            eth_wr,
            eth_tx,
            switch_tx,
            switch_rx,
            novlan,
            switch_fail
        );
        flat!(
            eth_mac,
            rx_code_err,
            rx_fcs_ok,
            rx_fcs_err,
            rx_good,
            rx_bad,
            rx_uc,
            rx_bc,
            tx_good,
            tx_uc,
            tx_mc,
            tx_bc,
            tx_undersize,
            tx_oversize
        );
        for (index, value) in self.rsm_drop_raw.iter().take(3).enumerate() {
            m.insert(format!("rsm_drop_raw[{index}]"), *value);
        }
        m
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PonStatsDelta {
    pub onu_index: u16,
    pub elapsed_ms: u64,
    /// False when ONU identity/generation or clock continuity cannot be established.
    pub continuity_verified: bool,
    /// Withheld unless continuity is verified; values are modulo 2^32.
    pub counters_modulo_4294967296: Option<BTreeMap<String, u32>>,
}

/// Compute modulo-2^32 deltas only when the two snapshots are from the same
/// ONU generation and the second is strictly later in wall-clock time.
pub fn delta(first: &PonStatsSnapshot, second: &PonStatsSnapshot) -> PonStatsDelta {
    let continuity_verified =
        first.onu_index == second.onu_index && first.started_unix_ms < second.started_unix_ms;
    let counters = continuity_verified.then(|| {
        let before = first.flat_counters();
        second
            .flat_counters()
            .into_iter()
            .filter_map(|(key, value)| before.get(&key).map(|old| (key, value.wrapping_sub(*old))))
            .collect()
    });
    PonStatsDelta {
        onu_index: second.onu_index,
        elapsed_ms: second.started_unix_ms.saturating_sub(first.started_unix_ms),
        continuity_verified,
        counters_modulo_4294967296: counters,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(onu_index: u16, started: u64, bip: u32) -> PonStatsSnapshot {
        let mut phy = BTreeMap::new();
        phy.insert("pon_sds_cfg".into(), serde_json::json!(0x1234));
        PonStatsSnapshot {
            schema_version: 1,
            onu_index,
            started_unix_ms: started,
            us_gtc: PonUsGtc {
                bip_err: bip,
                ..Default::default()
            },
            us_gem: Default::default(),
            ds_gem: Default::default(),
            onu_det: Default::default(),
            onu_us_rsm: Default::default(),
            onu_ds_pkt: Default::default(),
            rsm_eth_sizes: vec![0; 8],
            rsm_drop_raw: vec![1, 2, 3],
            nni_us: Default::default(),
            nni_ds: Default::default(),
            eth_mac: Default::default(),
            phy_cfg: phy,
        }
    }

    #[test]
    fn flat_counters_are_stable_and_exclude_configuration() {
        let s = snapshot(0, 1, 7);
        let m = s.flat_counters();
        assert_eq!(m.get("us_gtc.bip_err"), Some(&7));
        assert_eq!(m.get("rsm_drop_raw[2]"), Some(&3));
        assert!(m.get("rsm_drop_raw[3]").is_none());
        assert!(m.keys().all(|k| !k.starts_with("phy_cfg")));
        assert!(s.flat_counters() == m);
    }

    #[test]
    fn delta_wraps_modulo_and_withholds_on_identity_or_clock_regression() {
        let a = snapshot(0, 1000, u32::MAX - 5);
        let b = snapshot(0, 2500, 4);
        let d = delta(&a, &b);
        assert!(d.continuity_verified);
        assert_eq!(d.elapsed_ms, 1500);
        assert_eq!(
            d.counters_modulo_4294967296
                .as_ref()
                .and_then(|m| m.get("us_gtc.bip_err")),
            Some(&10)
        );

        let other = snapshot(1, 2500, 4);
        assert!(!delta(&a, &other).continuity_verified);
        assert!(delta(&a, &other).counters_modulo_4294967296.is_none());

        let earlier = snapshot(0, 900, 4);
        assert!(!delta(&a, &earlier).continuity_verified);
    }
}
