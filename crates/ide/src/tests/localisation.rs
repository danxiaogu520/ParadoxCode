use super::support::*;
use text::AbsPath;

fn multi_language_root(tag: &str) -> std::path::PathBuf {
    let root = temp_root(tag);
    let current = root.join("current");
    std::fs::create_dir_all(current.join("localisation/l_english")).expect("english dir");
    std::fs::create_dir_all(current.join("localisation/l_french")).expect("french dir");
    std::fs::write(
        current.join("localisation/l_english/a_l_english.yml"),
        "l_english:\nsearch_key:0 \"English text\"\n",
    )
    .expect("english localisation");
    std::fs::write(
        current.join("localisation/l_french/a_l_french.yml"),
        "l_french:\nsearch_key:0 \"Texte francais\"\nfrench_only:0 \"Seul\"\n",
    )
    .expect("french localisation");
    root
}

fn scan(root: &std::path::Path, preferred: &[String]) -> AnalysisHost {
    let mut host = eu4_host(game::eu4::bootstrap_rules());
    // Preferences are fixed before scanning, mirroring the LSP's
    // initialize-time configuration.
    host.set_preferred_localisation_languages(preferred.to_vec());
    host.apply_change(WorkspaceChange::SetSourceRoots(vec![SourceRoot::new(
        SourceRootId::new(1),
        SourceRootKind::Project,
        AbsPath::normalize(&root.join("current")),
    )]));
    host.refresh_source_roots().expect("scan roots");
    host
}

fn search(
    host: &AnalysisHost,
    key: Option<&str>,
    value: Option<&str>,
) -> crate::LocalisationSearchResult {
    crate::localisation_search_with_cancellation(
        &host.snapshot(),
        key,
        value,
        crate::LocalisationKeyMatch::Substring,
        10,
        &CancellationToken::new(),
    )
    .expect("localisation search")
}

/// One hit per key, sited at the target language's effective definition;
/// keys defined only in other languages stay listed without a value.
#[test]
fn localisation_search_sites_hits_at_the_target_language() {
    let root = multi_language_root("loc-search");
    let host = scan(&root, &[]);

    let result = search(&host, Some("search_key"), None);
    assert_eq!(result.hits.len(), 1, "one hit per key: {:?}", result.hits);
    assert_eq!(result.hits[0].value.as_deref(), Some("English text"));
    assert_eq!(result.hits[0].language.as_deref(), Some("l_english"));

    let result = search(&host, Some("french_only"), None);
    assert_eq!(result.hits.len(), 1, "the key itself stays findable");
    assert_eq!(
        result.hits[0].value, None,
        "keys defined only outside the target language carry no value"
    );
    assert_eq!(result.hits[0].language.as_deref(), Some("l_french"));

    let result = search(&host, None, Some("texte"));
    assert!(
        result.hits.is_empty(),
        "value search only ever sees the target language"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}

/// The first configured preference is the search's target language.
#[test]
fn localisation_search_follows_the_configured_target_language() {
    let root = multi_language_root("loc-search-fr");
    let host = scan(&root, &["french".to_owned()]);

    let result = search(&host, Some("search_key"), None);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].value.as_deref(), Some("Texte francais"));
    assert_eq!(result.hits[0].language.as_deref(), Some("l_french"));

    let result = search(&host, Some("french_only"), None);
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].value.as_deref(), Some("Seul"));

    let result = search(&host, None, Some("english text"));
    assert!(
        result.hits.is_empty(),
        "value search only ever sees the target language"
    );

    std::fs::remove_dir_all(root).expect("cleanup");
}
