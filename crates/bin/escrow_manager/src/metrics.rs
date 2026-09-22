use lazy_static::lazy_static;
use prometheus::{
    register_gauge, register_gauge_vec, register_histogram, register_int_counter,
    register_int_gauge, Gauge, GaugeVec, Histogram, IntCounter, IntGauge,
};

lazy_static! {
    pub static ref METRICS: Metrics = Metrics::new();
}

pub struct Metrics {
    pub total_debt_grt: Gauge,
    pub total_balance_grt: Gauge,
    pub total_target_grt: Gauge,
    pub total_adjustment_grt: Gauge,
    pub total_thawing_grt: Gauge,
    pub receiver_count: IntGauge,
    pub thawing_count: IntGauge,
    pub loop_duration: Histogram,
    pub deposit: ResponseMetrics,
    pub thaw: ResponseMetrics,
    pub withdraw: ResponseMetrics,
    // Per-receiver metrics
    pub debt_grt: GaugeVec,
    pub balance_grt: GaugeVec,
    pub target_grt: GaugeVec,
    pub adjustment_grt: GaugeVec,
    pub thawing_grt: GaugeVec,
}

impl Metrics {
    fn new() -> Self {
        Self {
            total_debt_grt: register_gauge!(
                "escrow_total_debt_grt",
                "total outstanding debt across all receivers in GRT"
            )
            .unwrap(),
            total_balance_grt: register_gauge!(
                "escrow_total_balance_grt",
                "total escrow balance across all receivers in GRT"
            )
            .unwrap(),
            total_target_grt: register_gauge!(
                "escrow_total_target_grt",
                "total target escrow balance across all receivers in GRT"
            )
            .unwrap(),
            total_adjustment_grt: register_gauge!(
                "escrow_total_adjustment_grt",
                "total GRT deposited in the last cycle"
            )
            .unwrap(),
            total_thawing_grt: register_gauge!(
                "escrow_total_thawing_grt",
                "total escrow pending withdrawal across all receivers in GRT"
            )
            .unwrap(),
            receiver_count: register_int_gauge!(
                "escrow_receiver_count",
                "number of receivers being tracked"
            )
            .unwrap(),
            thawing_count: register_int_gauge!(
                "escrow_thawing_count",
                "number of receivers with escrow pending withdrawal"
            )
            .unwrap(),
            loop_duration: register_histogram!(
                "escrow_loop_duration_seconds",
                "duration of each polling cycle in seconds"
            )
            .unwrap(),
            deposit: ResponseMetrics::new("escrow_deposit", "escrow deposit transaction"),
            thaw: ResponseMetrics::new("escrow_thaw", "escrow thaw transaction"),
            withdraw: ResponseMetrics::new("escrow_withdraw", "escrow withdrawal transaction"),
            debt_grt: register_gauge_vec!(
                "escrow_debt_grt",
                "outstanding debt per receiver in GRT",
                &["receiver"]
            )
            .unwrap(),
            balance_grt: register_gauge_vec!(
                "escrow_balance_grt",
                "escrow balance per receiver in GRT",
                &["receiver"]
            )
            .unwrap(),
            target_grt: register_gauge_vec!(
                "escrow_target_grt",
                "target escrow balance per receiver in GRT",
                &["receiver"]
            )
            .unwrap(),
            adjustment_grt: register_gauge_vec!(
                "escrow_adjustment_grt",
                "last adjustment per receiver in GRT",
                &["receiver"]
            )
            .unwrap(),
            thawing_grt: register_gauge_vec!(
                "escrow_thawing_grt",
                "escrow pending withdrawal per receiver in GRT",
                &["receiver"]
            )
            .unwrap(),
        }
    }
}

#[derive(Clone)]
pub struct ResponseMetrics {
    pub ok: IntCounter,
    pub err: IntCounter,
    pub duration: Histogram,
}

impl ResponseMetrics {
    pub fn new(prefix: &str, description: &str) -> Self {
        let metrics = Self {
            ok: register_int_counter!(
                &format!("{prefix}_ok"),
                &format!("{description} success count"),
            )
            .unwrap(),
            err: register_int_counter!(
                &format!("{prefix}_err"),
                &format!("{description} error count"),
            )
            .unwrap(),
            duration: register_histogram!(
                &format!("{prefix}_duration"),
                &format!("{description} duration"),
            )
            .unwrap(),
        };
        metrics.ok.inc();
        metrics.err.inc();
        metrics
    }
}
