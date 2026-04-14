// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn classify_path_detects_known_content_types() {
    let service = IndexingService;

    assert_eq!(service.classify_path("notes.txt"), ContentClass::Text);
    assert_eq!(service.classify_path("image.png"), ContentClass::Image);
    assert_eq!(service.classify_path("lib.rs"), ContentClass::SourceCode);
    assert_eq!(service.classify_path("archive.tar"), ContentClass::Archive);
    assert_eq!(service.classify_path("blob.bin"), ContentClass::Binary);
}
