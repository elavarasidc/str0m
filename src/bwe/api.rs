//! Bandwidth estimation.

use std::time::{Duration, Instant};

use crate::{Rtc, rtp_::Mid};

pub use crate::rtp_::Bitrate;

/// Invalid configuration of the evaluation-only external controller.
#[cfg(feature = "_internal_test_exports")]
#[derive(Debug, PartialEq, Eq)]
pub enum ExternalBweError {
    /// A leaky-bucket pacer must have been enabled with `enable_bwe`.
    Disabled,
    /// External estimates must be positive.
    ZeroBitrate,
}

#[cfg(feature = "_internal_test_exports")]
impl std::fmt::Display for ExternalBweError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Disabled => "external controller requires enable_bwe",
            Self::ZeroBitrate => "external estimate must be positive",
        })
    }
}

#[cfg(feature = "_internal_test_exports")]
impl std::error::Error for ExternalBweError {}

/// Per-packet transport feedback from the remote receiver.
///
/// Reports contain retained send records updated by a TWCC report, in transport sequence order.
/// Previously acknowledged packets are omitted. A reported loss may occur in multiple batches
/// and may later be acknowledged. Unreported or expired send records are not included.
#[derive(Debug, Clone, PartialEq)]
pub struct TwccPacketReport {
    /// Extended transport-wide sequence number, not an RTP stream sequence number.
    pub seq: u64,
    /// Sender-local packet send time.
    pub send_time: Instant,
    /// Sender-local arrival time of the feedback, including feedback reporting packet loss.
    ///
    /// Present for every emitted record. This is not the receiver's packet arrival time or a
    /// loss indicator.
    pub local_recv_time: Option<Instant>,
    /// Reconstructed receiver packet arrival time, or `None` for reported loss.
    ///
    /// This clock is anchored to the first feedback arrival, not synchronized with the sender.
    /// Differences between receiver timestamps are meaningful; subtracting `send_time` does not
    /// give absolute one-way delay.
    pub remote_recv_time: Option<Instant>,
    /// RTP body size, including any RTX sequence prefix or padding, excluding headers and SRTP.
    ///
    /// This is neither wire size nor necessarily useful media payload size.
    pub size_bytes: usize,
    /// Whether the packet belongs to a native BWE probe cluster.
    pub is_probe: bool,
    /// Whether the packet was sent on an audio media section.
    ///
    /// `false` does not identify a specific non-audio packet type.
    pub is_audio: bool,
    /// Send-to-feedback-arrival duration, which can also be present for reported loss.
    ///
    /// Includes receiver reporting delay; this is not an unbiased network RTT measurement.
    pub rtt: Option<Duration>,
}

impl TwccPacketReport {
    pub(crate) fn from_record(record: &crate::rtp_::TwccSendRecord) -> Self {
        Self {
            seq: *record.seq(),
            send_time: record.local_send_time(),
            local_recv_time: record.local_recv_time(),
            remote_recv_time: record.remote_recv_time(),
            size_bytes: record.size(),
            is_probe: record.cluster().is_some(),
            is_audio: record.is_audio(),
            rtt: record.rtt(),
        }
    }
}

#[derive(Debug, PartialEq)]
#[non_exhaustive]
/// Bandwidth estimation kind.
pub enum BweKind {
    /// Transport wide congestion control.
    ///
    /// Emits the current estimate with `can_probe: true` when probing becomes
    /// available, initially using the configured starting bitrate. Estimate
    /// updates follow while probing is available. On loss of probing capability,
    /// emits the last reported estimate once with `can_probe: false`, then
    /// suppresses updates until probing becomes available again.
    Twcc {
        /// Estimated available bitrate. Initially this is the configured starting bitrate.
        /// When probing becomes unavailable, this retains the last estimate.
        estimate: Bitrate,
        /// Whether the local BWE can currently probe for capacity.
        ///
        /// Requires BWE to be enabled, SRTP keys, and a sending media section
        /// supporting TWCC feedback and the transport sequence extension.
        /// This does not guarantee that feedback is arriving.
        can_probe: bool,
    },
    /// REMB (Receiver Estimated Maximum Bitrate)
    Remb {
        /// Bitrate reported by the remote receiver.
        estimate: Bitrate,
        /// Media section associated with the report.
        mid: Mid,
    },
}

/// Access to the Bandwidth Estimate subsystem.
pub struct Bwe<'a>(pub(crate) &'a mut Rtc);

impl<'a> Bwe<'a> {
    /// Configured pacing and padding rates, not measured wire throughput.
    /// Returns `None` when the session has no leaky-bucket pacer.
    #[cfg(feature = "_internal_test_exports")]
    pub fn pacing_rates(&self) -> Option<(Bitrate, Bitrate)> {
        self.0.session.pacing_rates()
    }

    /// Replace native estimation with an application-supplied estimate for evaluation.
    ///
    /// Requires `enable_bwe` and a positive bitrate. Once selected, native delay/loss
    /// estimates and probe results cannot change the rate, including after `reset`.
    /// The shared pacer, ALR detector and probe scheduler use the external estimate.
    /// External controllers have no native overuse signal, so padding is not suppressed
    /// by native delay detection. This policy difference must be recorded by evaluations.
    ///
    /// The application must also control its source rate, schedule inference and handle
    /// errors/stale feedback. This API provides no fallback to GCC. TWCC estimate events
    /// after selection report the supplied value, without native smoothing.
    #[cfg(feature = "_internal_test_exports")]
    pub fn set_external_estimate(&mut self, estimate: Bitrate) -> Result<(), ExternalBweError> {
        self.0.session.set_external_bwe_estimate(estimate)
    }

    /// Enable or disable [`Event::TwccFeedback`][crate::Event::TwccFeedback] events.
    ///
    /// Disabled by default. Disabling discards queued packet feedback; enabling again starts
    /// with subsequent reports, not a replay of previous reports.
    ///
    /// This only enables observation. It does not negotiate TWCC, enable receiver feedback,
    /// enable native BWE, or change pacing or bitrate control. Events also work without native
    /// BWE when TWCC is negotiated and feedback is received. Native BWE, if enabled, continues
    /// processing the same records.
    pub fn set_twcc_feedback_events(&mut self, enabled: bool) {
        self.0.session.set_twcc_feedback_events(enabled);
    }

    /// Configure the desired bitrate.
    ///
    /// Configure the bandwidth estimation system with the desired bitrate.
    /// **Note:** This only has an effect if BWE has been enabled via
    /// [`RtcConfig::enable_bwe`][crate::RtcConfig::enable_bwe].
    ///
    /// * `desired_bitrate` The bitrate you would like to eventually send at. The BWE system will try
    ///   to reach this bitrate by probing with padding packets. You should allocate your media bitrate
    ///   based on the estimated the BWE system produces via
    ///   [`Event::EgressBitrateEstimate`][crate::Event::EgressBitrateEstimate]. This rate might not
    ///   be reached if the network link cannot sustain the desired bitrate.
    ///
    /// ## Example
    ///
    /// Say you have three simulcast video tracks each with a high layer configured at 1.5Mbit/s.
    /// You should then set the desired bitrate to 4.5Mbit/s (or slightly higher). If the network
    /// link can sustain 4.5Mbit/s there will eventually be an
    /// [`Event::EgressBitrateEstimate`][crate::Event::EgressBitrateEstimate] with this estimate.
    pub fn set_desired_bitrate(&mut self, desired_bitrate: Bitrate) {
        self.0.session.set_bwe_desired_bitrate(desired_bitrate);
    }

    /// Reset the BWE with a new init_bitrate
    ///
    /// This discards the current estimator state and starts estimation again
    /// from `init_bitrate`. Normal transitions from audio-only to video sending
    /// do not require a reset: with TWCC negotiated, probing can discover
    /// capacity before video starts. The desired bitrate controls the capacity
    /// the estimator attempts to discover.
    pub fn reset(&mut self, init_bitrate: Bitrate) {
        self.0.session.reset_bwe(init_bitrate);
    }
}
