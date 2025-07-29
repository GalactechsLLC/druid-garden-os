use crate::database::map_sqlx_error;
use argon2::password_hash::{Salt, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use log::error;
use portfu_admin::users::UserRole;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use sha2::digest::Output;
use sha2::{Digest, Sha256, Sha256VarCore};
use sqlx::{FromRow, Sqlite, SqlitePool, Transaction};
use std::io::{Error, ErrorKind};
use std::sync::Arc;

#[derive(Debug, FromRow, Deserialize, Serialize)]
pub struct UserWithInfoWithPassword {
    pub id: i64,
    pub username: String,
    pub password: Vec<u8>,
    pub role: UserRole,
}

#[derive(Debug, FromRow, Deserialize, Serialize)]
pub struct UsernameWithPassword {
    pub username: String,
    pub password: Vec<u8>,
}
#[derive(Debug, FromRow, Deserialize, Serialize)]
pub struct UserPasswordUpdate {
    pub username: String,
    pub old_password: String,
    pub new_password: String,
}

pub async fn has_no_users(pool: &SqlitePool) -> Result<bool, Error> {
    match sqlx::query_scalar!(
        r#"
        SELECT count(*)
        FROM users
        "#
    )
    .fetch_one(pool)
    .await
    {
        Ok(o) => Ok(o == 0),
        Err(e) => Err(map_sqlx_error(e)),
    }
}

pub async fn login(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserWithInfoWithPassword>, Error> {
    match sqlx::query_as!(
        UserWithInfoWithPassword,
        r#"
        SELECT id, username, password, role
        FROM users
        WHERE username = $1
        "#,
        username
    )
    .fetch_optional(pool)
    .await
    {
        Ok(o) => Ok(o),
        Err(e) => Err(map_sqlx_error(e)),
    }
}

pub async fn update_password(
    pool: &SqlitePool,
    argon: Arc<Argon2<'static>>,
    data: UserPasswordUpdate,
) -> Result<bool, Error> {
    let mut tx: Transaction<Sqlite> = pool.begin().await.map_err(map_sqlx_error)?;
    //Validate Existing Password
    let existing = match sqlx::query_as!(
        UserWithInfoWithPassword,
        r#"
        SELECT id, username, password, role
        FROM users
        WHERE username = $1
        "#,
        data.username
    )
    .fetch_optional(tx.as_mut())
    .await
    {
        Ok(o) => o,
        Err(e) => return Err(map_sqlx_error(e)),
    };
    match existing {
        None => Err(Error::new(ErrorKind::NotFound, "User not found")),
        Some(user) => {
            let pch_string = String::from_utf8_lossy(&user.password).to_string();
            let hash = PasswordHash::new(pch_string.as_ref()).map_err(|e| {
                error!("{e:?}");
                Error::new(ErrorKind::NotFound, "User not found")
            })?;
            if argon
                .verify_password(data.old_password.as_ref(), &hash)
                .is_err()
            {
                return Err(Error::new(ErrorKind::NotFound, "User not found"));
            }
            //Convert the new password to a hash
            let mut hasher = Sha256::new();
            hasher.update(user.id.to_be_bytes().as_ref());
            let mut buf = [0u8; 32];
            hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
            let mut rng = ChaCha20Rng::from_seed(buf);
            let salt = SaltString::generate(&mut rng);
            let pass_hash = argon
                .hash_password(data.new_password.as_ref(), Salt::from(&salt))
                .map_err(|e| Error::other(format!("{e:?}")))?;
            let hash_pch_bytes = pass_hash.serialize();
            let hash_bytes = hash_pch_bytes.as_bytes();
            //Set the New Password
            sqlx::query!(
                r#"
                UPDATE users SET password = $1 WHERE id = $2
                "#,
                hash_bytes,
                user.id,
            )
            .execute(tx.as_mut())
            .await
            .map_err(map_sqlx_error)?;
            tx.commit().await.map_err(map_sqlx_error)?;
            Ok(true)
        }
    }
}

pub async fn register(
    pool: &SqlitePool,
    argon: &Argon2<'static>,
    data: UserWithInfoWithPassword,
) -> Result<Option<UserWithInfoWithPassword>, Error> {
    let mut tx: Transaction<Sqlite> = pool.begin().await.map_err(map_sqlx_error)?;
    let exists = sqlx::query_scalar!(
        r#"
        SELECT 1
        FROM users
        WHERE username = $1
        LIMIT 1
        "#,
        data.username
    )
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    if exists.is_some() {
        // We have an existing user, rollback transaction, return error
        tx.rollback().await.map_err(map_sqlx_error)?;
        return Err(Error::new(ErrorKind::AlreadyExists, "User already exists"));
    }
    //Insert User, Have the Password Be random until we calc the Hash
    let fake_pass = rand::thread_rng().gen::<[u8; 32]>().to_vec();
    let role_str = data.role.to_string();
    let user_id: i64 = sqlx::query_scalar!(
        r#"
        INSERT INTO users (username, password, role)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
        data.username,
        fake_pass,
        role_str
    )
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    //Convert the password to a hash
    let mut hasher = Sha256::new();
    hasher.update(user_id.to_be_bytes().as_ref());
    let mut buf = [0u8; 32];
    hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
    let mut rng = ChaCha20Rng::from_seed(buf);
    let salt = SaltString::generate(&mut rng);
    let pass_hash = argon
        .hash_password(data.password.as_ref(), Salt::from(&salt))
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let hash_pch_bytes = pass_hash.serialize();
    let hash_bytes = hash_pch_bytes.as_bytes();
    sqlx::query!(
        r#"
        UPDATE users SET password = $1 WHERE id = $2
        "#,
        hash_bytes,
        user_id,
    )
    .execute(tx.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    let new_user = sqlx::query_as!(
        UserWithInfoWithPassword,
        r#"
        SELECT id, username, password, role
        FROM users
        WHERE id = $1
        "#,
        user_id
    )
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_sqlx_error)?;
    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(Some(new_user))
}
