// Copyright (c) 2026 Christian Möller
// SPDX-License-Identifier: MIT

use crate::config::StorageTier;
use crate::domain::inode::Inode;
use crate::error::CoreFsResult;

const POINTER_PREFIX: &str = "@pointer:";

#[derive(Debug, Default)]
pub struct MetadataService;

impl MetadataService {
    pub fn set_attribute(inode: &mut Inode, key: &str, value: &str) {
        if let Some((_, existing)) = inode
            .metadata
            .attributes
            .iter_mut()
            .find(|(existing_key, _)| existing_key == key)
        {
            *existing = value.to_string();
        } else {
            inode
                .metadata
                .attributes
                .push((key.to_string(), value.to_string()));
        }
    }

    pub fn add_tag(inode: &mut Inode, tag: &str) {
        if !inode.metadata.tags.iter().any(|existing| existing == tag) {
            inode.metadata.tags.push(tag.to_string());
            inode.metadata.tags.sort();
        }
    }

    pub fn remove_tag(inode: &mut Inode, tag: &str) {
        inode.metadata.tags.retain(|existing| existing != tag);
    }

    pub fn set_storage_tier(inode: &mut Inode, tier: StorageTier) {
        inode.metadata.storage_tier = tier;
    }

    pub fn set_content_pointer(inode: &mut Inode, key: &str, target_path: &str, extractor: &str) {
        Self::set_attribute(
            inode,
            key,
            &format!("{POINTER_PREFIX}{target_path}|{extractor}"),
        );
    }

    pub fn resolve_attributes(
        inode: &Inode,
        resolve_content: impl Fn(&str) -> CoreFsResult<Vec<u8>>,
    ) -> Vec<(String, String)> {
        inode
            .metadata
            .attributes
            .iter()
            .map(|(key, value)| {
                let resolved = match Self::decode_pointer(value) {
                    Some((target_path, extractor)) => resolve_content(target_path)
                        .map(|bytes| apply_extractor(&bytes, extractor))
                        .unwrap_or_else(|_| value.clone()),
                    None => value.clone(),
                };
                (key.clone(), resolved)
            })
            .collect()
    }

    pub fn is_pointer_value(value: &str) -> bool {
        value.starts_with(POINTER_PREFIX)
    }

    fn decode_pointer(value: &str) -> Option<(&str, &str)> {
        let payload = value.strip_prefix(POINTER_PREFIX)?;
        payload.split_once('|')
    }
}

fn apply_extractor(bytes: &[u8], extractor: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    match extractor {
        "full" => text.into_owned(),
        value if value.starts_with("summary-") => {
            let length = value
                .trim_start_matches("summary-")
                .parse::<usize>()
                .unwrap_or(160);
            summarize(&text, length)
        }
        _ => summarize(&text, 160),
    }
}

fn summarize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.trim().to_string()
    } else {
        let summary = text.chars().take(max_chars).collect::<String>();
        format!("{}...", summary.trim_end())
    }
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
