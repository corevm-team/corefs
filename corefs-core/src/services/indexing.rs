// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use crate::domain::metadata::ContentClass;

#[derive(Debug, Default)]
pub struct IndexingService;

impl IndexingService {
    pub fn classify_path(&self, path: &str) -> ContentClass {
        if path.ends_with(".txt") || path.ends_with(".md") {
            ContentClass::Text
        } else if path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".jpeg") {
            ContentClass::Image
        } else if path.ends_with(".rs")
            || path.ends_with(".c")
            || path.ends_with(".cpp")
            || path.ends_with(".py")
        {
            ContentClass::SourceCode
        } else if path.ends_with(".zip") || path.ends_with(".tar") || path.ends_with(".gz") {
            ContentClass::Archive
        } else {
            ContentClass::Binary
        }
    }
}

#[cfg(test)]
#[path = "indexing_tests.rs"]
mod tests;
