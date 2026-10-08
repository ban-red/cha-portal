use cha_client::NodeStats;
use cha_ui_spec::health::Platform;
use cha_ui_spec::panel::{PanelHealth, build_panel, spec};
use cha_ui_spec::panel_cases::format_cases;

use super::*;
use crate::health;

fn panel_of(s: &StatsSnapshot) -> Panel {
    build_panel(
        spec(),
        &s.values(),
        &PanelHealth::smooth(),
        Platform::Native,
    )
}

#[test]
fn the_compact_line_names_what_it_has() {
    let s = StatsSnapshot {
        codec: "Hevc".into(),
        transport_tag: "WT",
        present_fps: 59.8,
        latency_ms: Some(11.64),
        mbps: Some(58.21),
        reconnects: 2,
        ..StatsSnapshot::default()
    };
    assert_eq!(
        panel_of(&s).compact,
        "60 fps · 11.6 ms · 58.2 Mbit/s · HEVC/WT · 2 reconnects"
    );
    let bare = StatsSnapshot::default();
    assert_eq!(panel_of(&bare).compact, "0 fps · – ms · – Mbit/s");
    assert_eq!(
        panel_of(&StatsSnapshot {
            reconnects: 1,
            ..bare
        })
        .compact,
        "0 fps · – ms · – Mbit/s · 1 reconnect"
    );
}

#[test]
fn the_report_has_the_numbers_around_the_issues() {
    let s = StatsSnapshot {
        codec: "Hevc".into(),
        transport_tag: "WT",
        width: 2560,
        height: 1440,
        present_fps: 58.0,
        target_fps: Some(60),
        mbps: Some(60.0),
        latency_ms: Some(30.0),
        decode_ms: Some(3.0),
        rtt_ms: Some(2.0),
        lost: Some(4),
        dropped: 7,
        node: Some(NodeStats {
            cpu: 12.0,
            mem_used: 8 << 30,
            mem_total: 32 << 30,
            gpu: Some(50.0),
            ..NodeStats::default()
        }),
        ..StatsSnapshot::default()
    };
    let mut history = vec![s.clone(); 8];
    for (i, h) in history.iter_mut().enumerate() {
        h.dropped = i as u64 * 7;
    }
    let health = health::assess(&history);
    assert!(health.issues.iter().any(|i| i.id == "dropped"));
    let panel = model(&s, &health);
    let text = issue_report(&panel, &health.issues[..1], "Cha Player 9, macOS 99");
    assert!(text.starts_with(&format!("{}: ", health.issues[0].title)));
    assert!(text.contains("\nHealth: "));
    assert!(text.contains("Stream: HEVC/WT, 2560×1440, 58 of 60 fps, 60.0 Mbit/s"));
    assert!(text.contains("Network: round trip 2.0 ms, 4 lost, 7 dropped"));
    assert!(text.contains("Node: CPU 12%, RAM 8.0/32.0 GB, GPU 50%"));
    assert!(text.ends_with("Player: Cha Player 9, macOS 99"));
}

/// A reading with every field the player has, so every key the spec gives it is filled.
fn full() -> StatsSnapshot {
    StatsSnapshot {
        width: 2560,
        height: 1440,
        codec: "PyroWave444".into(),
        transport_tag: "WT",
        present_fps: 60.0,
        decode_fps: 60.0,
        target_fps: Some(60),
        mbps: Some(60.0),
        decode_ms: Some(3.0),
        latency_ms: Some(10.0),
        rtt_ms: Some(2.0),
        lost: Some(1),
        recovered: Some(2),
        dropped: 3,
        decode_errors: 4,
        partial: 5,
        audio_buffer_ms: Some(30.0),
        audio_underruns: 6,
        audio_dropped_ms: 7,
        reconnects: 1,
        node: Some(NodeStats {
            cpu: 30.0,
            cores: 16,
            load1: 2.0,
            mem_used: 8,
            mem_total: 32,
            gpu: Some(50.0),
            vram_used: Some(4),
            vram_total: Some(12),
            enc: Some(20.0),
            dec: Some(1.0),
            temp: Some(70.0),
            power: Some(188.0),
            power_limit: Some(320.0),
            clock: Some(1980.0),
            streamer_cpu: 40.0,
        }),
        ..StatsSnapshot::default()
    }
}

#[test]
fn the_player_fills_every_value_the_spec_gives_it_and_nothing_else() {
    let values = full().values();
    let mut filled: Vec<&str> = values.keys().collect();
    filled.sort_unstable();
    let mut wanted: Vec<&str> = spec()
        .values
        .iter()
        .filter(|(_, v)| v.platforms.contains(&Platform::Native) && !v.derived)
        .map(|(k, _)| k.as_str())
        .collect();
    wanted.sort_unstable();
    assert_eq!(filled, wanted);
}

#[test]
fn a_transport_without_a_reading_leaves_its_key_out() {
    let v = StatsSnapshot::default().values();
    for k in [
        "rtt_ms",
        "lost",
        "recovered",
        "audio_jitter_ms",
        "partial",
        "width",
        "node_cpu",
    ] {
        assert!(!v.contains(k), "{k}");
    }
    assert!(v.contains("dropped"));
}

#[test]
fn every_spec_section_has_a_pref_to_fold_it() {
    for s in &spec().sections {
        sections::section_of(&s.id);
    }
    assert_eq!(spec().sections.len(), Section::ALL.len());
}

#[test]
fn the_codec_tag_cases() {
    for c in format_cases().codec_tag.iter().filter(|c| {
        c.platforms
            .as_ref()
            .is_none_or(|p| p.contains(&Platform::Native))
    }) {
        let tag = match c.transport.as_deref() {
            Some("webtransport") => "WT",
            Some("webrtc") => "RTC",
            _ => "",
        };
        let s = StatsSnapshot {
            codec: c.codec.clone().unwrap_or_default(),
            transport_tag: tag,
            ..StatsSnapshot::default()
        };
        assert_eq!(s.codec_text(), c.expect, "{}", c.name);
    }
}

fn click(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: winit::event::MouseButton::Left,
    }
}

#[test]
fn a_press_on_the_panel_is_the_panels_until_it_is_released() {
    let mut panel = StatsPanel::new(OverlayPrefs::default());
    panel.rect = Some(Rect::from_min_size(
        egui::pos2(10.0, 10.0),
        vec2(100.0, 50.0),
    ));
    let inside = Some(egui::pos2(20.0, 20.0));
    let outside = Some(egui::pos2(500.0, 500.0));
    assert!(!panel.consumes(outside, &click(ElementState::Pressed)));
    assert!(!panel.consumes(outside, &click(ElementState::Released)));
    assert!(panel.consumes(inside, &click(ElementState::Pressed)));
    // Dragged out of the panel: still the panel's.
    let moved = WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    };
    assert!(panel.consumes(outside, &moved));
    assert!(panel.consumes(outside, &click(ElementState::Released)));
    assert!(!panel.consumes(outside, &moved));
}

#[test]
fn preferences_save_only_once_they_settle() {
    let mut panel = StatsPanel::new(OverlayPrefs::default());
    assert!(panel.take_save(false).is_none());
    panel.toggle_open();
    assert!(panel.take_save(false).is_none(), "just changed");
    assert!(panel.save_due().is_some());
    let saved = panel.take_save(true).expect("forced");
    assert!(!saved.open);
    assert!(panel.take_save(true).is_none());
    assert!(panel.save_due().is_none());
}
