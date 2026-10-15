//! Folders for saved connections and saved queries: a slash in a name files
//! it, so `Clients/Acme production` is "Acme production" in "Clients".

/// A name's folder (empty for none) and the name shown inside it.
pub(crate) fn split(name: &str) -> (&str, &str) {
    match name.rsplit_once('/') {
        Some((folder, leaf)) if !folder.trim().is_empty() && !leaf.trim().is_empty() => {
            (folder.trim(), leaf.trim())
        }
        _ => ("", name),
    }
}

/// `items` grouped by folder: those in none first, then each folder by name,
/// keeping their order within it.
pub(crate) fn group<'a, T>(
    items: impl IntoIterator<Item = &'a T>,
    name: impl Fn(&T) -> &str,
) -> Vec<(String, Vec<&'a T>)> {
    let mut groups: Vec<(String, Vec<&'a T>)> = Vec::new();
    for item in items {
        let folder = split(name(item)).0.to_string();
        match groups.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, members)) => members.push(item),
            None => groups.push((folder, vec![item])),
        }
    }
    groups.sort_by(|(a, _), (b, _)| {
        (!a.is_empty(), a.to_lowercase()).cmp(&(!b.is_empty(), b.to_lowercase()))
    });
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slash_files_a_name() {
        assert_eq!(
            split("Clients/Acme production"),
            ("Clients", "Acme production")
        );
        assert_eq!(split("Clients/Acme/EU"), ("Clients/Acme", "EU"));
        assert_eq!(split("shop.db"), ("", "shop.db"));
        // A slash with nothing on one side isn't a folder.
        assert_eq!(split("/tmp"), ("", "/tmp"));
        assert_eq!(split("weird/"), ("", "weird/"));
    }

    #[test]
    fn unfiled_first_then_folders_by_name() {
        let names = ["b/one", "plain", "a/two", "b/three", "other"];
        let groups = group(&names, |n| n);
        let shape: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|(f, items)| (f.as_str(), items.iter().map(|n| **n).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                ("", vec!["plain", "other"]),
                ("a", vec!["a/two"]),
                ("b", vec!["b/one", "b/three"]),
            ]
        );
    }
}
