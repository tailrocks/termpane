// SPDX-FileCopyrightText: 2026 Alexey Zhokhov
// SPDX-License-Identifier: Apache-2.0

//! An OSC 8 hyperlink costs memory once, however many cells it covers, and an
//! over-long one is refused.
//!
//! Every test feeds bytes through `DamageGrid::process` and reads cells back
//! through the public API.

use termpane::DamageGrid;

const URI_MAX: usize = 8 * 1024;
const ID_MAX: usize = 256;

fn open(id: &str, uri: &str) -> Vec<u8> {
    format!("\x1b]8;id={id};{uri}\x1b\\").into_bytes()
}

fn uri_of_length(len: usize) -> String {
    let prefix = "http://x/";
    format!("{prefix}{}", "A".repeat(len - prefix.len()))
}

fn link_uri(grid: &DamageGrid, row: u16, col: u16) -> Option<String> {
    grid.cell(row, col)
        .and_then(|cell| cell.hyperlink.as_ref())
        .map(|link| link.uri.clone())
}

fn link_uri_address(grid: &DamageGrid, row: u16, col: u16) -> Option<usize> {
    grid.cell(row, col)
        .and_then(|cell| cell.hyperlink.as_ref())
        .map(|link| link.uri.as_ptr() as usize)
}

#[test]
fn every_cell_under_one_link_shares_the_link() {
    let mut grid = DamageGrid::new(5, 80, 0);
    grid.process(&open("a", &uri_of_length(4_000)));
    grid.process("x".repeat(200).as_bytes());

    let addresses: Vec<usize> = (0..200_u16)
        .filter_map(|n| link_uri_address(&grid, n / 80, n % 80))
        .collect();
    assert_eq!(addresses.len(), 200, "every printed cell carries the link");
    assert!(
        addresses.iter().all(|&address| address == addresses[0]),
        "cells must share one copy of the link, not own one each"
    );
}

#[test]
fn a_link_at_the_cap_is_accepted() {
    let mut grid = DamageGrid::new(2, 20, 0);
    let uri = uri_of_length(URI_MAX);
    grid.process(&open(&"i".repeat(ID_MAX), &uri));
    grid.process(b"ok");
    assert_eq!(link_uri(&grid, 0, 0).map(|link| link.len()), Some(URI_MAX));
    assert_eq!(
        grid.hyperlink_target_at_content_row(0, 1).map(str::len),
        Some(URI_MAX)
    );
}

#[test]
fn a_uri_over_the_cap_is_refused_and_the_text_still_prints() {
    for len in [URI_MAX + 1, 140_000] {
        let mut grid = DamageGrid::new(2, 20, 0);
        grid.process(&open("", &uri_of_length(len)));
        grid.process(b"text");
        assert_eq!(
            grid.cell(0, 0).map(|cell| cell.contents().to_owned()),
            Some("t".to_owned()),
            "{len}"
        );
        assert!(
            link_uri(&grid, 0, 0).is_none(),
            "a {len} byte uri must be refused"
        );
        assert!(
            grid.hyperlink_target_at_content_row(0, 0).is_none(),
            "a {len} byte uri must have no hover target"
        );
    }
}

#[test]
fn an_id_over_the_cap_is_refused() {
    let mut grid = DamageGrid::new(2, 20, 0);
    grid.process(&open(&"i".repeat(ID_MAX + 1), "http://x/"));
    grid.process(b"text");
    assert!(link_uri(&grid, 0, 0).is_none());
}

#[test]
fn a_refused_link_also_ends_the_link_before_it() {
    let mut grid = DamageGrid::new(2, 20, 0);
    grid.process(&open("a", "http://a/"));
    grid.process(b"x");
    grid.process(&open("b", &uri_of_length(URI_MAX + 1)));
    grid.process(b"y");
    assert_eq!(link_uri(&grid, 0, 0).as_deref(), Some("http://a/"));
    assert!(link_uri(&grid, 0, 1).is_none());
}

#[test]
fn a_cell_keeps_its_link_after_thousands_of_other_links() {
    let mut grid = DamageGrid::new(100, 200, 0);
    for n in 0..10_000_u32 {
        grid.process(&open(&format!("n{n}"), &format!("http://h/{n}")));
        grid.process(b"x");
    }
    for n in [0_u32, 1, 8_191, 8_192, 9_999] {
        let (row, col) = ((n / 200) as u16, (n % 200) as u16);
        let expected = format!("http://h/{n}");
        assert_eq!(
            grid.hyperlink_target_at_content_row(usize::from(row), col),
            Some(expected.as_str()),
            "{n}"
        );
    }
}
