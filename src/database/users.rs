use crate::database::map_sqlx_error;
use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use portfu_admin::users::UserRole;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::sync::Semaphore;

static PASSWORD_WORKERS: Semaphore = Semaphore::const_new(2);

#[derive(Debug, Deserialize, Serialize)]
pub struct UserWithInfoWithPassword {
    pub id: i64,
    pub username: String,
    pub password: Vec<u8>,
    pub role: UserRole,
}

pub struct AuthenticationRecord {
    pub user: UserWithInfoWithPassword,
    pub generation: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UsernameWithPassword {
    pub username: String,
    pub password: Vec<u8>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct UserPasswordUpdate {
    pub username: String,
    pub old_password: String,
    pub new_password: String,
}

#[derive(Debug, Serialize)]
pub struct AccountSummary {
    pub id: i64,
    pub username: String,
    pub role: UserRole,
}

type RawUser = (i64, String, Vec<u8>, String, i64);
fn record((id, username, password, role, generation): RawUser) -> AuthenticationRecord {
    AuthenticationRecord {
        user: UserWithInfoWithPassword {
            id,
            username,
            password,
            role: UserRole::from(role),
        },
        generation,
    }
}

pub fn validate_username(username: &str) -> Result<(), Error> {
    if username.is_empty()
        || username.len() > 128
        || username.trim() != username
        || username.chars().any(char::is_control)
    {
        return Err(Error::new(ErrorKind::InvalidInput, "Username must contain 1 to 128 bytes without surrounding whitespace or control characters"));
    }
    Ok(())
}
pub fn validate_new_password(password: &[u8]) -> Result<(), Error> {
    if password.len() < 12 || password.len() > 1024 || password.iter().all(u8::is_ascii_whitespace)
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Password must contain 12 to 1024 bytes",
        ));
    }
    Ok(())
}

pub async fn hash_password(argon: Argon2<'static>, password: Vec<u8>) -> Result<Vec<u8>, Error> {
    let permit = PASSWORD_WORKERS
        .try_acquire()
        .map_err(|_| Error::new(ErrorKind::WouldBlock, "Password service busy; retry later"))?;
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let salt = SaltString::generate(&mut OsRng);
        argon
            .hash_password(&password, &salt)
            .map(|hash| hash.to_string().into_bytes())
            .map_err(|error| Error::other(error.to_string()))
    })
    .await
    .map_err(Error::other)?;
    result
}
pub async fn verify_password(
    argon: Argon2<'static>,
    password: Vec<u8>,
    encoded: Vec<u8>,
) -> Result<bool, Error> {
    if password.len() > 1024 {
        return Ok(false);
    }
    let permit = PASSWORD_WORKERS
        .try_acquire()
        .map_err(|_| Error::new(ErrorKind::WouldBlock, "Password service busy; retry later"))?;
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let encoded = std::str::from_utf8(&encoded).map_err(Error::other)?;
        let hash =
            PasswordHash::new(encoded).map_err(|_| Error::other("Invalid stored password hash"))?;
        Ok(argon.verify_password(&password, &hash).is_ok())
    })
    .await
    .map_err(Error::other)?;
    result
}

pub async fn has_no_users(pool: &SqlitePool) -> Result<bool, Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await
        .map(|count| count == 0)
        .map_err(map_sqlx_error)
}

pub async fn authentication_record(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<AuthenticationRecord>, Error> {
    sqlx::query_as::<_, RawUser>(
        "SELECT id, username, password, role, auth_generation FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(record))
    .map_err(map_sqlx_error)
}

pub async fn update_password(
    pool: &SqlitePool,
    argon: Arc<Argon2<'static>>,
    data: UserPasswordUpdate,
    expected_generation: i64,
) -> Result<bool, Error> {
    validate_new_password(data.new_password.as_bytes())?;
    let existing = authentication_record(pool, &data.username)
        .await?
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Invalid credentials"))?;
    if existing.generation != expected_generation {
        return Err(Error::new(ErrorKind::PermissionDenied, "Session revoked"));
    }
    if !verify_password(
        (*argon).clone(),
        data.old_password.into_bytes(),
        existing.user.password,
    )
    .await?
    {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Invalid credentials",
        ));
    }
    let hash = hash_password((*argon).clone(), data.new_password.into_bytes()).await?;
    let changed = sqlx::query("UPDATE users SET password = ?, auth_generation = auth_generation + 1 WHERE id = ? AND auth_generation = ?")
        .bind(hash).bind(existing.user.id).bind(existing.generation).execute(pool).await.map_err(map_sqlx_error)?.rows_affected();
    if changed != 1 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Account changed; sign in again",
        ));
    }
    Ok(true)
}

pub async fn register(
    pool: &SqlitePool,
    argon: &Argon2<'static>,
    data: UserWithInfoWithPassword,
) -> Result<Option<UserWithInfoWithPassword>, Error> {
    validate_username(&data.username)?;
    validate_new_password(&data.password)?;
    let hash = hash_password(argon.clone(), data.password).await?;
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO users (username, password, role) VALUES (?, ?, ?) RETURNING id",
    )
    .bind(&data.username)
    .bind(&hash)
    .bind(data.role.to_string())
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_error)?;
    Ok(Some(UserWithInfoWithPassword {
        id,
        username: data.username,
        password: hash,
        role: data.role,
    }))
}

pub async fn list_accounts(pool: &SqlitePool) -> Result<Vec<AccountSummary>, Error> {
    sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, username, role FROM users ORDER BY id LIMIT 1000",
    )
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|(id, username, role)| AccountSummary {
                id,
                username,
                role: UserRole::from(role),
            })
            .collect()
    })
    .map_err(map_sqlx_error)
}

pub fn can_change_role(actor: UserRole, target: UserRole, requested: UserRole) -> bool {
    actor == UserRole::SuperAdmin
        || (actor >= UserRole::Admin && target < actor && requested < actor)
}

pub async fn change_role(
    pool: &SqlitePool,
    actor_id: i64,
    actor_generation: i64,
    target_id: i64,
    requested: UserRole,
) -> Result<bool, Error> {
    let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
    // Actor and target are checked in the same SQLite snapshot as the update.
    // A concurrent writer makes snapshot promotion fail rather than bypass checks.
    let actor =
        sqlx::query_as::<_, (String, i64)>("SELECT role, auth_generation FROM users WHERE id = ?")
            .bind(actor_id)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_sqlx_error)?
            .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Invalid account"))?;
    if actor.1 != actor_generation {
        return Err(Error::new(ErrorKind::PermissionDenied, "Session revoked"));
    }
    let target = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE id = ?")
        .bind(target_id)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_sqlx_error)?
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "Account not found"))?;
    let target = UserRole::from(target);
    if !can_change_role(UserRole::from(actor.0), target, requested) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Role change exceeds your authority",
        ));
    }
    if target == UserRole::SuperAdmin && requested != UserRole::SuperAdmin {
        let count =
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE role = 'SuperAdmin'")
                .fetch_one(tx.as_mut())
                .await
                .map_err(map_sqlx_error)?;
        if count <= 1 {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "Cannot demote the last SuperAdmin",
            ));
        }
    }
    sqlx::query("UPDATE users SET role = ?, auth_generation = auth_generation + 1 WHERE id = ?")
        .bind(requested.to_string())
        .bind(target_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_sqlx_error)?;
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT UNIQUE NOT NULL, password BLOB NOT NULL, role TEXT NOT NULL, auth_generation INTEGER NOT NULL DEFAULT 0)").execute(&pool).await.unwrap();
        pool
    }
    #[test]
    fn role_ceiling_blocks_peer_and_self_escalation() {
        assert!(!can_change_role(
            UserRole::Admin,
            UserRole::User,
            UserRole::Admin
        ));
        assert!(!can_change_role(
            UserRole::Admin,
            UserRole::Admin,
            UserRole::User
        ));
        assert!(!can_change_role(
            UserRole::Manager,
            UserRole::User,
            UserRole::Viewer
        ));
        assert!(can_change_role(
            UserRole::Admin,
            UserRole::User,
            UserRole::Manager
        ));
    }
    #[tokio::test]
    async fn protects_last_owner_and_revokes_role_sessions() {
        let pool = pool().await;
        sqlx::query("INSERT INTO users VALUES (1, 'owner', x'00', 'SuperAdmin', 0), (2, 'viewer', x'00', 'Viewer', 0)").execute(&pool).await.unwrap();
        assert!(change_role(&pool, 1, 0, 1, UserRole::Viewer).await.is_err());
        assert!(change_role(&pool, 1, 0, 2, UserRole::SuperAdmin)
            .await
            .unwrap());
        assert!(change_role(&pool, 1, 0, 1, UserRole::Viewer).await.unwrap());
        assert!(change_role(&pool, 1, 0, 2, UserRole::User).await.is_err());
        let generation: i64 = sqlx::query_scalar("SELECT auth_generation FROM users WHERE id=2")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(generation, 1);
    }
    #[tokio::test]
    async fn password_hashes_use_fresh_salts() {
        let password = b"a long test password".to_vec();
        let first = hash_password(Argon2::default(), password.clone())
            .await
            .unwrap();
        let second = hash_password(Argon2::default(), password.clone())
            .await
            .unwrap();
        assert_ne!(first, second);
        assert!(verify_password(Argon2::default(), password, first)
            .await
            .unwrap());
        let pool = pool().await;
        sqlx::query("INSERT INTO users VALUES (1, 'test', ?, 'SuperAdmin', 0)")
            .bind(second)
            .execute(&pool)
            .await
            .unwrap();
        let change = || UserPasswordUpdate {
            username: "test".into(),
            old_password: "a long test password".into(),
            new_password: "a different long password".into(),
        };
        assert!(
            update_password(&pool, Arc::new(Argon2::default()), change(), 1)
                .await
                .is_err()
        );
        assert!(
            update_password(&pool, Arc::new(Argon2::default()), change(), 0)
                .await
                .unwrap()
        );
        let account = authentication_record(&pool, "test").await.unwrap().unwrap();
        assert_eq!(account.generation, 1);
        assert!(!verify_password(
            Argon2::default(),
            b"a long test password".to_vec(),
            account.user.password.clone()
        )
        .await
        .unwrap());
        assert!(verify_password(
            Argon2::default(),
            b"a different long password".to_vec(),
            account.user.password
        )
        .await
        .unwrap());
    }
}
