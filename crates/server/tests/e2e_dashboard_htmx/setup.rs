use super::*;

use llmroller_store::users::{NewUser, create_user};

pub(crate) async fn setup_dashboard_test_db()
-> (tempfile::TempDir, sqlx::SqlitePool, MasterKey, UserId) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("dashboard_test.db");

    let writer_pool = open_pool(&db_path, PoolRole::Writer)
        .await
        .expect("writer pool");
    migrate(&writer_pool).await.expect("migrate");

    let entropy = OsEntropy;
    let master_key = MasterKey::from_bytes([0xBB; 32]);
    load_or_initialize(&writer_pool, "key-0", &master_key, Arc::new(entropy), 1)
        .await
        .expect("instance load");
    seed_defaults(&writer_pool, None, 1)
        .await
        .expect("seed settings");

    let system_actor = ActorScope::system(SystemTask::Writer, SystemSeal::store());
    let admin = create_user(
        &writer_pool,
        &system_actor,
        NewUser {
            username: "admin_dash",
            password: "supersecretadminpass123",
            role: Role::Admin,
            must_change_password: false,
        },
        &entropy,
        1,
    )
    .await
    .expect("admin");

    (dir, writer_pool, master_key, admin.id)
}

/// Assert an HTMX partial rendered user-controlled text as inert markup (SEC-19).
pub(crate) fn assert_no_raw_user_markup(body: &str, context: &str) {
    assert!(
        !body.contains("<script"),
        "{context} leaked a raw <script> tag"
    );
    assert!(!body.contains("<img"), "{context} leaked a raw <img> tag");
    assert!(
        body.contains("&lt;script&gt;"),
        "{context} did not render the payload escaped"
    );
}
