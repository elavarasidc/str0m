use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use str0m::bwe::{Bitrate, BweKind};
use str0m::format::Codec;
use str0m::media::{Direction, MediaKind};
use str0m::rtp::rtcp::Twcc;
use str0m::{Event, Rtc, RtcError};
use tracing::info_span;

mod common;
use common::{TestRtc, init_crypto_default, init_log, negotiate, progress};

#[test]
pub fn twcc() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let l_rtc = Rtc::builder().enable_raw_packets(true).build(now);
    let r_rtc = Rtc::builder().enable_raw_packets(true).build(now);

    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_rtc);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_rtc);

    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());

    let mid = negotiate(&mut l, &mut r, |change| {
        change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None)
    });

    loop {
        if l.is_connected() || r.is_connected() {
            break;
        }
        progress(&mut l, &mut r)?;
    }

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    let params = l.params_vp8();
    assert_eq!(params.spec().codec, Codec::Vp8);
    let pt = params.pt();

    let data_a = [1_u8; 80];

    loop {
        {
            let wallclock = l.start + l.duration();
            let time = l.duration().into();
            l.writer(mid).unwrap().write(pt, wallclock, time, data_a)?;
        }

        progress(&mut l, &mut r)?;

        if l.duration() > Duration::from_secs(10) {
            break;
        }
    }

    let (sent_twcc, received_twcc) = {
        use str0m::rtp::{RawPacket, rtcp::Rtcp};
        let r_twcc: Vec<_> = r
            .events
            .iter()
            .filter_map(|(_, e)| {
                if let Some(RawPacket::RtcpTx(Rtcp::Twcc(twcc))) = e.as_raw_packet() {
                    Some(twcc)
                } else {
                    None
                }
            })
            .collect();

        let l_twcc: Vec<_> = l
            .events
            .iter()
            .filter_map(|(_, e)| {
                if let Some(RawPacket::RtcpRx(Rtcp::Twcc(twcc))) = e.as_raw_packet() {
                    Some(twcc)
                } else {
                    None
                }
            })
            .collect();
        (r_twcc, l_twcc)
    };

    assert!(!sent_twcc.is_empty(), "Should've sent TWCC");
    assert!(
        sent_twcc.len() == received_twcc.len(),
        "The number of TWCC packets received should match what was sent"
    );
    assert!(
        sent_twcc == received_twcc,
        "The same TWCC packets that were sent should be received"
    );

    let sent_is_consecutive = sent_twcc
        .iter()
        .fold((true, None), |(consecutive, last), packet| {
            let consecutive = consecutive
                && last
                    .map(|l: &Twcc| l.feedback_count.wrapping_add(1) == packet.feedback_count)
                    .unwrap_or(true);

            (consecutive, Some(packet))
        })
        .0;

    assert!(!sent_twcc.is_empty(), "Should have sent some TWCC");
    assert!(
        sent_is_consecutive,
        "Sent TWCC packets should contain no gaps in terms of `feedback_count`"
    );

    Ok(())
}

fn run_packet_feedback(
    now: Instant,
    enabled: Option<bool>,
    native_bwe: bool,
) -> Result<TestRtc, RtcError> {
    init_log();
    init_crypto_default();
    let l_rtc = Rtc::builder()
        .enable_raw_packets(true)
        .enable_bwe(native_bwe.then_some(Bitrate::kbps(300)))
        .build(now);
    let r_rtc = Rtc::builder().enable_raw_packets(true).build(now);
    let mut l = TestRtc::new_with_rtc(info_span!("L"), l_rtc);
    let mut r = TestRtc::new_with_rtc(info_span!("R"), r_rtc);
    l.start = now;
    l.last = now;
    r.start = now;
    r.last = now;
    if let Some(enabled) = enabled {
        l.bwe().set_twcc_feedback_events(enabled);
    }
    if native_bwe {
        l.bwe().set_desired_bitrate(Bitrate::mbps(1));
    }
    l.add_host_candidate((Ipv4Addr::new(1, 1, 1, 1), 1000).into());
    r.add_host_candidate((Ipv4Addr::new(2, 2, 2, 2), 2000).into());
    // Use one stream for native A/A; mixed-media send order varies even with reporting off.
    let (audio, video) = negotiate(&mut l, &mut r, |change| {
        (
            (!native_bwe)
                .then(|| change.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None)),
            change.add_media(MediaKind::Video, Direction::SendOnly, None, None, None),
        )
    });
    while !l.is_connected() || !r.is_connected() {
        assert!(
            l.duration() < Duration::from_secs(5),
            "connection timed out"
        );
        progress(&mut l, &mut r)?;
    }
    let start = l.last.max(r.last);
    l.last = start;
    r.last = start;
    let audio_pt = l.params_opus().pt();
    let video_pt = l.params_vp8().pt();
    while l.last < start + Duration::from_secs(3) {
        let wallclock = l.last;
        let time = l.duration().into();
        if let Some(audio) = audio {
            l.writer(audio)
                .unwrap()
                .write(audio_pt, wallclock, time, [0xf8, 0xff, 0xfe])?;
        }
        l.writer(video)
            .unwrap()
            .write(video_pt, wallclock, time, [1_u8; 80])?;
        progress(&mut l, &mut r)?;
    }
    Ok(l)
}

#[test]
fn packet_feedback_is_disabled_by_default_and_can_be_explicitly_disabled() -> Result<(), RtcError> {
    for enabled in [None, Some(false)] {
        let l = run_packet_feedback(Instant::now(), enabled, false)?;
        assert!(
            !l.events
                .iter()
                .any(|(_, e)| matches!(e, Event::TwccFeedback(_)))
        );
        assert!(l.events.iter().any(|(_, e)| matches!(
            e.as_raw_packet(),
            Some(str0m::rtp::RawPacket::RtcpRx(str0m::rtp::rtcp::Rtcp::Twcc(
                _
            )))
        )));
    }
    Ok(())
}

#[test]
fn packet_feedback_works_without_native_bwe() -> Result<(), RtcError> {
    let l = run_packet_feedback(Instant::now(), Some(true), false)?;
    let batches: Vec<_> = l
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::TwccFeedback(packets) => Some(packets),
            _ => None,
        })
        .collect();
    assert!(!batches.is_empty());
    assert!(batches.iter().all(|batch| !batch.is_empty()));
    assert!(
        batches
            .iter()
            .all(|batch| batch.windows(2).all(|p| p[0].seq < p[1].seq))
    );
    let packets: Vec<_> = batches.into_iter().flatten().collect();
    assert!(packets.iter().any(|p| p.is_audio));
    assert!(packets.iter().any(|p| !p.is_audio));
    assert!(packets.iter().all(|p| p.local_recv_time.is_some()));
    assert!(packets.iter().all(|p| p.remote_recv_time.is_some()));
    assert!(packets.iter().all(|p| p.size_bytes > 0 && !p.is_probe));
    assert!(
        !l.events
            .iter()
            .any(|(_, e)| matches!(e, Event::EgressBitrateEstimate(_)))
    );
    Ok(())
}

#[test]
fn packet_feedback_preserves_native_estimates_and_send_schedule() -> Result<(), RtcError> {
    let now = Instant::now();
    let disabled = run_packet_feedback(now, None, true)?;
    let enabled = run_packet_feedback(now, Some(true), true)?;
    let estimates = |rtc: &TestRtc| {
        rtc.events
            .iter()
            .filter_map(|(time, event)| match event {
                Event::EgressBitrateEstimate(BweKind::Twcc {
                    estimate,
                    can_probe,
                }) => Some((*time - now, *estimate, *can_probe)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let sends = |rtc: &TestRtc| {
        rtc.events
            .iter()
            .filter_map(|(time, event)| match event.as_raw_packet() {
                Some(str0m::rtp::RawPacket::RtpTx(_, data)) => Some((*time - now, data.len())),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let native_estimates = estimates(&disabled);
    let native_sends = sends(&disabled);
    assert!(native_estimates.len() > 1);
    assert!(!native_sends.is_empty());
    let observed_sends = sends(&enabled);
    for (i, (native, observed)) in native_sends.iter().zip(&observed_sends).enumerate() {
        assert_eq!(native, observed, "native RTP send {i} changed");
    }
    assert_eq!(native_sends.len(), observed_sends.len());
    assert_eq!(native_estimates, estimates(&enabled));
    assert!(enabled.events.iter().any(|(_, event)| matches!(
        event,
        Event::TwccFeedback(packets) if packets.iter().any(|p| p.is_probe)
    )));
    Ok(())
}
