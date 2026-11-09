//! WP23 · byte-bounded stdout framing: no panics on multibyte boundaries, no
//! fabricated frames from dropped prefixes, bounded memory, valid frames after
//! floods. Pure fixtures — no subprocesses.

use wagglepost::runner::{FrameSplitter, LINE_CAP};

fn push_all(parts: &[&[u8]]) -> (Vec<String>, u64) {
    let mut sp = FrameSplitter::default();
    let mut out = Vec::new();
    let mut dropped = 0u64;
    for p in parts {
        sp.push(p, &mut out, &mut dropped);
    }
    sp.finish(&mut out, &mut dropped);
    (out, dropped)
}

#[test]
fn framing_normal_frames_pass_through() {
    let (out, dropped) = push_all(&[b"{\"a\":1}\n{\"b\":2}\r\n"]);
    assert_eq!(out, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    assert_eq!(dropped, 0);
}

#[test]
fn framing_multibyte_straddling_cap_never_panics_and_drops_whole_frame() {
    // a frame of 'é' (2 bytes) repeated: a byte-boundary cut inside a char must
    // never be converted — and the overlong frame is dropped WHOLE, not truncated
    let unit = "é".as_bytes(); // 2 bytes
    let mut frame = Vec::new();
    for _ in 0..(LINE_CAP / 2 + 10) {
        frame.extend_from_slice(unit);
    }
    frame.push(b'\n');
    assert!(frame.len() > LINE_CAP);
    let (out, dropped) = push_all(&[&frame, b"after\n"]);
    assert_eq!(dropped, 1, "the overlong frame is dropped whole");
    assert_eq!(out, vec!["after".to_string()], "the next valid frame survives");
}

#[test]
fn framing_frame_exactly_at_cap_is_kept() {
    let body = "x".repeat(LINE_CAP - 1); // after \r-strip semantics: keep == cap boundary
    let mut frame = body.clone().into_bytes();
    frame.push(b'\n');
    let (out, dropped) = push_all(&[&frame]);
    assert_eq!(dropped, 0);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].len(), LINE_CAP - 1);
}

#[test]
fn framing_overlong_fake_tail_then_real_frame() {
    // >2 MiB unterminated junk, then a real line, then EOF: the junk tail must
    // NEVER be emitted as a frame of its own; the real line survives
    let junk = vec![b'z'; LINE_CAP * 2 + 777];
    let (out, dropped) = push_all(&[&junk, b"\nreal-frame\n"]);
    assert!(dropped >= 1, "the fake tail is dropped (head guard + overlong frame)");
    assert_eq!(out, vec!["real-frame".to_string()], "no fabricated frame from the tail");
}

#[test]
fn framing_no_newline_eof_small_tail_emitted() {
    let (out, dropped) = push_all(&[b"partial frame without newline"]);
    assert_eq!(out, vec!["partial frame without newline".to_string()]);
    assert_eq!(dropped, 0);
}

#[test]
fn framing_no_newline_eof_overlong_tail_dropped() {
    let tail = vec![b'w'; LINE_CAP + 1];
    let (out, dropped) = push_all(&[&tail]);
    assert!(out.is_empty(), "EOF tail over the cap is dropped whole");
    assert_eq!(dropped, 1);
}

#[test]
fn framing_across_chunks_and_crlf() {
    // one frame split across many chunk boundaries, with \r\n ending
    let full = b"{\"type\":\"session\",\"n\":42}\r\n";
    let mut sp = FrameSplitter::default();
    let mut out = Vec::new();
    let mut dropped = 0u64;
    for b in full.iter() {
        sp.push(std::slice::from_ref(b), &mut out, &mut dropped);
    }
    sp.finish(&mut out, &mut dropped);
    assert_eq!(out, vec!["{\"type\":\"session\",\"n\":42}".to_string()]);
    assert_eq!(dropped, 0);
}

#[test]
fn framing_empty_lines_are_not_emitted() {
    let (out, dropped) = push_all(&[b"\n\n\nreal\n"]);
    assert_eq!(out, vec!["real".to_string()]);
    assert_eq!(dropped, 0);
}

#[test]
fn framing_memory_bounded_under_flood_without_newlines() {
    // a flooder with NO newlines: the internal buffer must stay bounded (2×cap)
    let mut sp = FrameSplitter::default();
    let mut out = Vec::new();
    let mut dropped = 0u64;
    let chunk = vec![b'q'; 65536];
    for _ in 0..200 {
        // 200 * 64 KiB = 12.5 MiB of unterminated flood
        sp.push(&chunk, &mut out, &mut dropped);
    }
    assert!(out.is_empty(), "nothing unterminated is ever emitted");
    assert!(dropped >= 1, "flood drops are counted");
    // the buffer never exceeds the 2×cap guard (plus one chunk)
    assert!(sp.buffered_len() <= LINE_CAP * 2 + 65536, "buffer bounded: {}", sp.buffered_len());
}
