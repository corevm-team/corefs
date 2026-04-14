// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use corefs::cli;

fn main() {
    if let Err(error) = cli::run(std::env::args()) {
        eprintln!("corefs error: {error}");
        std::process::exit(1);
    }
}
