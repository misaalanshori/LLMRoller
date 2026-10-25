use clap::Parser;
use super::{Cli, Command};

    #[test]
    #[doc = "Partial: SEC-2"]
    fn rotate_master_key_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "rotate-master-key",
            "--old-key-env",
            "OLD_KEY",
            "--new-key-env",
            "NEW_KEY",
            "--new-key-id",
            "mk_next",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid CLI");
        match cli.command {
            Command::RotateMasterKey(args) => {
                assert_eq!(args.old_key_env, "OLD_KEY");
                assert_eq!(args.new_key_env, "NEW_KEY");
                assert_eq!(args.new_key_id, "mk_next");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected RotateMasterKey"),
        }
    }

    #[test]
    #[doc = "Spec: AUTH-5"]
    fn reset_admin_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "reset-admin",
            "--username",
            "admin",
            "--data-dir",
            "/tmp/data",
        ])
        .expect("valid reset-admin CLI");
        match cli.command {
            Command::ResetAdmin(args) => {
                assert_eq!(args.username, "admin");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
            }
            _ => panic!("expected ResetAdmin"),
        }
    }

    #[test]
    #[doc = "Spec: AUD-1, Spec: SEC-13"]
    fn verify_audit_log_parses_required_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "verify-audit-log",
            "--anchor-seq",
            "100",
            "--anchor-hash",
            "abcdef123456",
            "--data-dir",
            "/tmp/data",
            "--json",
        ])
        .expect("valid verify-audit-log CLI");
        match cli.command {
            Command::VerifyAuditLog(args) => {
                assert_eq!(args.anchor_seq, Some(100));
                assert_eq!(args.anchor_hash.as_deref(), Some("abcdef123456"));
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
                assert!(args.json);
            }
            _ => panic!("expected VerifyAuditLog"),
        }
    }

    #[test]
    #[doc = "Spec: SEC-2"]
    fn generate_master_key_parses_without_arguments() {
        let cli = Cli::try_parse_from(["llmroller", "generate-master-key"]).expect("valid CLI");
        match cli.command {
            Command::GenerateMasterKey => {}
            _ => panic!("expected GenerateMasterKey"),
        }
    }

    #[test]
    #[doc = "Spec: POR-1, Spec: REL-1"]
    fn serve_parses_arguments() {
        let cli = Cli::try_parse_from([
            "llmroller",
            "serve",
            "--listen",
            "127.0.0.1:8080",
            "--admin-listen",
            "127.0.0.1:8081",
            "--metrics-listen",
            "127.0.0.1:9090",
            "--data-dir",
            "/tmp/data",
            "--dev-insecure-cookies",
        ])
        .expect("valid serve CLI");
        match cli.command {
            Command::Serve(args) => {
                assert_eq!(args.listen, "127.0.0.1:8080");
                assert_eq!(args.admin_listen, "127.0.0.1:8081");
                assert_eq!(args.metrics_listen, "127.0.0.1:9090");
                assert_eq!(
                    args.data_dir.as_deref(),
                    Some(std::path::Path::new("/tmp/data"))
                );
                assert!(args.dev_insecure_cookies);
            }
            _ => panic!("expected Serve"),
        }
    }

