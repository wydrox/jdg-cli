use super::*;
use credentials::testing::{StoreMode, install};

/// Unikalny katalog na test (pid + licznik), usuwany przy drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("jdg-cli-root-{name}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let dir = self.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn resolve(
    env: Option<&str>,
    config: Option<&str>,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> JdgRoot {
    resolve_jdg_root(&JdgRootInputs {
        env,
        config,
        cwd,
        home,
    })
}

// --- kolejność reguł ---

#[test]
fn env_jdg_root_wins_over_config_and_cwd_with_data() {
    let tmp = TempDir::new("env");
    let cwd = tmp.dir("cwd");
    fs::create_dir_all(cwd.join("data")).unwrap();
    let root = resolve(
        Some("/srv/jdg-cli"),
        Some("/elsewhere"),
        Some(&cwd),
        Some(&tmp.0),
    );
    assert_eq!(
        root,
        JdgRoot {
            path: PathBuf::from("/srv/jdg-cli"),
            source: JdgRootSource::Env
        }
    );
}

#[test]
fn config_jdg_root_wins_over_cwd_with_data() {
    // Zapamiętany katalog jest pod tym samym kluczem co ustawiony ręcznie, więc wygrywa
    // z bieżącym katalogiem: przypadkowy `data/` w innym folderze nie zmienia bazy.
    let tmp = TempDir::new("config");
    let cwd = tmp.dir("cwd");
    fs::create_dir_all(cwd.join("data")).unwrap();
    let root = resolve(None, Some("/srv/jdg-cli"), Some(&cwd), Some(&tmp.0));
    assert_eq!(root.path, PathBuf::from("/srv/jdg-cli"));
    assert_eq!(root.source, JdgRootSource::Config);
}

#[test]
fn blank_jdg_root_values_are_ignored() {
    let tmp = TempDir::new("blank");
    let cwd = tmp.dir("cwd");
    let home = tmp.dir("home");
    let root = resolve(Some("  "), Some(""), Some(&cwd), Some(&home));
    assert_eq!(root.source, JdgRootSource::Home);
}

#[test]
fn cwd_with_database_is_the_root() {
    let tmp = TempDir::new("cwd-db");
    let cwd = tmp.dir("cwd");
    fs::write(cwd.join("jdg-cli.sqlite"), b"").unwrap();
    let root = resolve(None, None, Some(&cwd), Some(&tmp.0));
    assert_eq!(root.path, cwd);
    assert_eq!(root.source, JdgRootSource::Cwd);
}

#[test]
fn cwd_with_data_directory_is_the_root() {
    let tmp = TempDir::new("cwd-data");
    let cwd = tmp.dir("cwd");
    fs::create_dir_all(cwd.join("data")).unwrap();
    assert_eq!(
        resolve(None, None, Some(&cwd), Some(&tmp.0)).source,
        JdgRootSource::Cwd
    );
}

#[test]
fn cwd_jdg_cli_checkout_is_the_root() {
    let tmp = TempDir::new("cwd-pkg");
    let cwd = tmp.dir("cwd");
    fs::write(
        cwd.join("Cargo.toml"),
        "[package]\nname = \"jdg-cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    assert_eq!(
        resolve(None, None, Some(&cwd), Some(&tmp.0)).source,
        JdgRootSource::Cwd
    );
}

#[test]
fn data_file_or_other_package_does_not_make_cwd_the_root() {
    let tmp = TempDir::new("cwd-other");
    let cwd = tmp.dir("cwd");
    let home = tmp.dir("home");
    fs::write(cwd.join("data"), b"not a directory").unwrap();
    fs::write(
        cwd.join("Cargo.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let root = resolve(None, None, Some(&cwd), Some(&home));
    assert_eq!(root.source, JdgRootSource::Home);
}

#[test]
fn empty_cwd_without_config_uses_local_share_jdg() {
    let tmp = TempDir::new("home");
    let cwd = tmp.dir("cwd");
    let home = tmp.dir("home");
    let root = resolve(None, None, Some(&cwd), Some(&home));
    assert_eq!(root.path, home.join(".local").join("share").join("jdg-cli"));
    assert_eq!(root.source, JdgRootSource::Home);
}

#[test]
fn jdg_root_value_expands_tilde_and_resolves_relative_paths_against_cwd() {
    let home = Path::new("/Users/someone");
    let cwd = Path::new("/work");
    assert_eq!(
        resolve(Some("~/jdg-cli"), None, Some(cwd), Some(home)).path,
        PathBuf::from("/Users/someone/jdg-cli")
    );
    assert_eq!(
        resolve(Some("~"), None, Some(cwd), Some(home)).path,
        PathBuf::from("/Users/someone")
    );
    assert_eq!(
        resolve(None, Some("state/jdg-cli"), Some(cwd), Some(home)).path,
        PathBuf::from("/work/state/jdg-cli")
    );
}

// --- komunikat na stderr ---

#[test]
fn notice_only_for_remembered_or_home_root() {
    let cwd = Path::new("/work");
    let at = |path: &str, source| JdgRoot {
        path: PathBuf::from(path),
        source,
    };
    assert_eq!(
        jdg_root_notice(&at("/srv/jdg-cli", JdgRootSource::Env), Some(cwd)),
        None
    );
    assert_eq!(
        jdg_root_notice(&at("/work", JdgRootSource::Cwd), Some(cwd)),
        None
    );
    assert_eq!(
        jdg_root_notice(&at("/work", JdgRootSource::Config), Some(cwd)),
        None
    );
    let remembered =
        jdg_root_notice(&at("/srv/jdg-cli", JdgRootSource::Config), Some(cwd)).unwrap();
    assert!(remembered.contains("/srv/jdg-cli"), "{remembered}");
    assert!(remembered.contains(JDG_ROOT_KEY), "{remembered}");
    let home = jdg_root_notice(
        &at("/Users/x/.local/share/jdg-cli", JdgRootSource::Home),
        Some(cwd),
    )
    .unwrap();
    assert!(home.contains("/Users/x/.local/share/jdg-cli"), "{home}");
}

// --- zapamiętanie katalogu ---

#[test]
fn remember_jdg_root_writes_once_and_keeps_the_first_value() {
    let tmp = TempDir::new("remember");
    let (_store, _guard) = install(tmp.dir("config"), &[], StoreMode::Unavailable);
    assert!(remember_jdg_root(Path::new("/work/jdg-cli")).unwrap());
    assert!(!remember_jdg_root(Path::new("/other")).unwrap());
    let vars = read_jdg_env_file().unwrap();
    assert_eq!(
        vars.get(JDG_ROOT_KEY).map(String::as_str),
        Some("/work/jdg-cli")
    );
}

#[test]
fn remember_jdg_root_keeps_other_config_values() {
    let tmp = TempDir::new("remember-keep");
    let (_store, _guard) = install(tmp.dir("config"), &[], StoreMode::Unavailable);
    let mut vars = HashMap::new();
    vars.insert("JDG_LLM_MODEL".to_string(), "gemma".to_string());
    write_jdg_env_file(&vars).unwrap();
    assert!(remember_jdg_root(Path::new("/work/jdg-cli")).unwrap());
    let vars = read_jdg_env_file().unwrap();
    assert_eq!(vars.get("JDG_LLM_MODEL").map(String::as_str), Some("gemma"));
    assert_eq!(
        vars.get(JDG_ROOT_KEY).map(String::as_str),
        Some("/work/jdg-cli")
    );
}

// --- ścieżki w katalogu jdg-cli ---

#[test]
fn default_paths_live_under_the_jdg_root() {
    let tmp = TempDir::new("defaults");
    let _root = set_test_jdg_root(&tmp.0);
    assert_eq!(default_db_path(), tmp.0.join("jdg-cli.sqlite"));
    assert_eq!(
        default_mail_candidates_path(2026),
        tmp.0
            .join("data/mail-all-pdf-2026-pdfs")
            .join("candidates.jsonl")
    );
    assert_eq!(
        default_amazon_mail_candidates_path(2026),
        tmp.0
            .join("data/mail-amazon-2026-pdfs")
            .join("candidates.jsonl")
    );
    assert_eq!(
        default_saldeo_records_path(2026),
        tmp.0.join("data/saldeo-2026").join("records.jsonl")
    );
    assert_eq!(default_ksef_out_path(2026), tmp.0.join("data/ksef-2026"));
    assert_eq!(
        ksef_accounting_cache_path(2026),
        tmp.0.join("data/saldeo-2026").join("ksef_accounting.json")
    );
}

#[test]
fn relative_ksef_data_dir_is_resolved_against_the_jdg_root() {
    if std::env::var_os("KSEF_DATA_DIR").is_some() {
        return; // zmienna procesu ma pierwszeństwo przed plikiem konfiguracji
    }
    let tmp = TempDir::new("ksef-dir");
    let (_store, _guard) = install(tmp.dir("config"), &[], StoreMode::Unavailable);
    let jdg = tmp.dir("jdg-cli");
    let _root = set_test_jdg_root(&jdg);
    let mut vars = HashMap::new();
    vars.insert("KSEF_DATA_DIR".to_string(), "exports".to_string());
    write_jdg_env_file(&vars).unwrap();
    assert_eq!(
        configured_ksef_out_path(2026),
        jdg.join("exports").join("ksef-2026")
    );
}

#[test]
fn explicit_test_root_is_cleared_by_the_guard() {
    let tmp = TempDir::new("guard");
    {
        let _root = set_test_jdg_root(&tmp.0);
        assert_eq!(jdg_root(), tmp.0);
    }
    assert_eq!(jdg_root(), PathBuf::new());
    assert_eq!(default_db_path(), PathBuf::from("jdg-cli.sqlite"));
}

// --- source_path zapisany względnie ---

#[test]
fn relative_source_path_resolves_against_the_root() {
    let tmp = TempDir::new("source");
    let root = tmp.dir("jdg-cli");
    fs::create_dir_all(root.join("data/mail")).unwrap();
    fs::write(root.join("data/mail/x.pdf"), b"%PDF").unwrap();
    assert_eq!(
        resolve_source_path_in(&root, Path::new("data/mail/x.pdf")),
        root.join("data/mail/x.pdf")
    );
    // Brak pliku nigdzie: ścieżka w katalogu jdg-cli (do komunikatu błędu).
    assert_eq!(
        resolve_source_path_in(&root, Path::new("data/mail/missing.pdf")),
        root.join("data/mail/missing.pdf")
    );
    // Bezwzględna bez zmian.
    let absolute = root.join("data/mail/x.pdf");
    assert_eq!(resolve_source_path_in(&tmp.0, &absolute), absolute);
}

#[test]
fn relative_source_path_missing_in_root_falls_back_to_cwd() {
    // `Cargo.toml` istnieje względem katalogu testów (korzeń pakietu), nie w pustym katalogu jdg-cli.
    let tmp = TempDir::new("source-cwd");
    assert_eq!(
        resolve_source_path_in(&tmp.0, Path::new("Cargo.toml")),
        PathBuf::from("Cargo.toml")
    );
}

#[test]
fn upload_plan_finds_relative_mail_pdf_in_the_jdg_root() {
    let tmp = TempDir::new("upload");
    fs::create_dir_all(tmp.0.join("data/mail-all-pdf-2026-pdfs")).unwrap();
    fs::write(
        tmp.0.join("data/mail-all-pdf-2026-pdfs/abc_1_faktura.pdf"),
        b"%PDF-1.4",
    )
    .unwrap();
    let mut record = empty_record(SourceKind::Mail);
    record.source_path = Some("data/mail-all-pdf-2026-pdfs/abc_1_faktura.pdf".into());

    let outside = saldeo_sync_item_from_record("missing_saldeo", &record, vec!["mail".into()]);
    assert!(!outside.can_upload);

    let _root = set_test_jdg_root(&tmp.0);
    let inside = saldeo_sync_item_from_record("missing_saldeo", &record, vec!["mail".into()]);
    assert!(inside.can_upload);
    assert_eq!(inside.upload_status, "planned");
    // Zapisany source_path zostaje taki, jak w rekordzie.
    assert_eq!(inside.source_path, record.source_path);
}
