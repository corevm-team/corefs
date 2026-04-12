use crate::error::{CoreFsError, CoreFsResult};

pub(super) fn validate_path(path: &str) -> CoreFsResult<()> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(CoreFsError::InvalidInput(format!(
            "paths must be absolute and non-empty: {path}"
        )));
    }
    if path.len() > 16_384 {
        return Err(CoreFsError::InvalidInput(format!(
            "path exceeds supported limit: {path}"
        )));
    }
    if path != "/" && path.ends_with('/') {
        return Err(CoreFsError::InvalidInput(format!(
            "paths must not end with '/': {path}"
        )));
    }
    Ok(())
}

pub(super) fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

pub(super) fn direct_child_name(parent: &str, path: &str) -> Option<String> {
    if parent == "/" {
        let remainder = path.strip_prefix('/')?;
        if remainder.is_empty() || remainder.contains('/') {
            return None;
        }
        return Some(remainder.to_string());
    }

    let prefix = format!("{parent}/");
    let remainder = path.strip_prefix(&prefix)?;
    if remainder.is_empty() || remainder.contains('/') {
        return None;
    }
    Some(remainder.to_string())
}

pub(super) fn is_descendant_path(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len()
        && path.starts_with(prefix)
        && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

pub(super) fn rebase_path(path: &str, old_prefix: &str, new_prefix: &str) -> String {
    if path == old_prefix {
        new_prefix.to_string()
    } else {
        format!("{new_prefix}/{}", &path[old_prefix.len() + 1..])
    }
}
