use crate::database::users::{
    login, register, update_password, UserPasswordUpdate, UserWithInfoWithPassword,
    UsernameWithPassword,
};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{Salt, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use log::{error, warn};
use portfu::prelude::async_trait::async_trait;
use portfu::prelude::http::{HeaderName, HeaderValue, StatusCode};
use portfu::prelude::{Path, State};
use portfu::wrappers::sessions::Session;
use portfu_admin::auth::{BasicAuth, Claims};
use portfu_admin::users::UserRole;
use portfu_core::wrappers::{WrapperFn, WrapperResult};
use portfu_core::{FromRequest, Json, ServiceData};
use portfu_macros::{get, post, put};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::digest::Output;
use sha2::{Digest, Sha256, Sha256VarCore};
use sqlx::types::time::OffsetDateTime;
use sqlx::SqlitePool;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct BasicAuthHandle {
    pool: SqlitePool,
    argon: Argon2<'static>,
}
impl BasicAuthHandle {
    pub fn new(pool: SqlitePool, argon: Argon2<'static>) -> Self {
        Self { pool, argon }
    }
}
#[async_trait]
impl BasicAuth for BasicAuthHandle {
    async fn login<U: AsRef<str> + Send + Sync, P: AsRef<str> + Send + Sync>(
        &self,
        username: U,
        password: P,
        session: Arc<RwLock<Session>>,
    ) -> Result<Claims, Error> {
        //Get all Data before All Comparisons
        let username = username.as_ref();
        let password = password.as_ref();
        //Fake Data Generated Every Time
        let bad_password = format!("bad_{password}");
        let fake_salt = SaltString::generate(&mut OsRng);
        let fake_pass_hash = self
            .argon
            .hash_password(bad_password.as_ref(), Salt::from(&fake_salt))
            .map_err(|e| Error::other(format!("{e:?}")))?;
        let fake_hash_pch_bytes = fake_pass_hash.serialize();
        //Fetch the Data from the Database
        let maybe_user_info = login(&self.pool, username).await?;
        //Sleep a slightly random duration to make all requests take different times
        let range = rand::thread_rng().gen_range(50..150);
        tokio::time::sleep(std::time::Duration::from_millis(range)).await;
        let pch_string;
        let claims;
        let now = OffsetDateTime::now_utc().unix_timestamp() as usize;
        let (user_id, hash_to_use) = match maybe_user_info {
            Some(user_info) => {
                //Found a User, Do a real compare
                pch_string = String::from_utf8_lossy(&user_info.password).to_string();
                claims = Claims {
                    aud: "localhost".to_string(),
                    exp: now + 30 * 60, //30 Minutes
                    iat: now,
                    iss: "localhost".to_string(),
                    nbf: now,
                    sub: user_info.id.to_string(),
                    eml: user_info.username,
                    rol: user_info.role,
                    org: vec![],
                };
                (
                    user_info.id,
                    PasswordHash::new(pch_string.as_ref()).map_err(|e| {
                        error!("{e:?}");
                        Error::new(ErrorKind::NotFound, "User not found")
                    })?,
                )
            }
            None => {
                //Do a fake comparison
                pch_string = String::from_utf8_lossy(fake_hash_pch_bytes.as_bytes()).to_string();
                claims = Claims {
                    aud: "localhost".to_string(),
                    exp: now + 30 * 60, //30 Minutes
                    iat: now,
                    iss: "localhost".to_string(),
                    nbf: now,
                    sub: "".to_string(),
                    eml: "".to_string(),
                    rol: UserRole::None,
                    org: vec![],
                };
                (
                    -1,
                    PasswordHash::new(pch_string.as_ref()).map_err(|e| {
                        error!("{e:?}");
                        Error::new(ErrorKind::NotFound, "User not found")
                    })?,
                )
            }
        };
        let mut hasher = Sha256::new();
        hasher.update(user_id.to_be_bytes().as_ref());
        let mut buf = [0u8; 32];
        hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
        let mut rng = ChaCha20Rng::from_seed(buf);
        let salt = SaltString::generate(&mut rng);
        let default_hash = self
            .argon
            .hash_password(b"Admin", &salt)
            .map_err(|e| Error::other(format!("{e:?}")))
            .map(|v| v.to_owned())?;
        if self
            .argon
            .verify_password(password.as_ref(), &hash_to_use)
            .is_ok()
        {
            if default_hash == hash_to_use {
                if let Some(require_update) = session.read().await.data.get::<RequireUpdate>() {
                    require_update.0.store(true, Ordering::Relaxed);
                    return Ok(claims);
                }
                session
                    .write()
                    .await
                    .data
                    .insert(RequireUpdate(Arc::new(AtomicBool::new(true))));
            }
            Ok(claims)
        } else {
            Err(Error::new(ErrorKind::NotFound, "User not found"))
        }
    }
}

#[post("/api/users/register", output = "none", eoutput = "bytes")]
pub async fn register_endpoint(
    pool: State<SqlitePool>,
    argon: State<Argon2<'static>>,
    data: Json<Option<UsernameWithPassword>>,
) -> Result<bool, Error> {
    if let Some(data) = data.inner() {
        register(
            pool.as_ref(),
            &argon.0,
            UserWithInfoWithPassword {
                id: -1,
                username: data.username,
                password: data.password,
                role: UserRole::User,
            },
        )
        .await
        .map(|v| v.is_some())
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            "Failed to register User, Invalid Input",
        ))
    }
}

#[put("/api/users/password", output = "none", eoutput = "bytes")]
pub async fn user_update_password(
    pool: State<SqlitePool>,
    argon: State<Argon2<'static>>,
    session: State<RwLock<Session>>,
    data: Json<Option<UserPasswordUpdate>>,
) -> Result<bool, Error> {
    if let Some(data) = data.inner() {
        let res = update_password(pool.as_ref(), argon.0.clone(), data).await?;
        if let Some(update_required) = session.0.read().await.data.get::<RequireUpdate>() {
            update_required.0.store(!res, Ordering::Relaxed);
        }
        Ok(res)
    } else {
        Err(Error::new(
            ErrorKind::InvalidData,
            "Failed to register User, Invalid Input",
        ))
    }
}

#[get("/api/users/password/{username}", output = "json", eoutput = "bytes")]
pub async fn user_requires_password_update(
    pool: State<SqlitePool>,
    argon: State<Argon2<'static>>,
    username: Path,
) -> Result<bool, Error> {
    match login(pool.as_ref(), username.inner().as_ref()).await? {
        None => Ok(false),
        Some(user) => {
            let mut hasher = Sha256::new();
            hasher.update(user.id.to_be_bytes().as_ref());
            let mut buf = [0u8; 32];
            hasher.finalize_into(<&mut Output<Sha256VarCore>>::from(&mut buf));
            let mut rng = ChaCha20Rng::from_seed(buf);
            let salt = SaltString::generate(&mut rng);
            let default_hash = argon
                .0
                .hash_password(b"Admin", &salt)
                .map_err(|e| Error::other(format!("{e:?}")))
                .map(|v| v.to_owned())?;
            let pch_string = String::from_utf8_lossy(&user.password).to_string();
            let user_hash = PasswordHash::new(pch_string.as_ref()).map_err(|e| {
                error!("{e:?}");
                Error::new(ErrorKind::NotFound, "User not found")
            })?;
            Ok(default_hash == user_hash)
        }
    }
}

#[derive(Clone)]
pub struct RequireUpdate(pub Arc<AtomicBool>);

pub struct PasswordUpdateWrapper {}
#[async_trait]
impl WrapperFn for PasswordUpdateWrapper {
    fn name(&self) -> &str {
        "PasswordUpdateWrapper"
    }

    async fn before(&self, data: &mut ServiceData) -> WrapperResult {
        match State::<RwLock<Session>>::from_request(&mut data.request, "update_required").await {
            Err(e) => {
                warn!("Failed to Find User Session for Password Update Redirect: {e:?}");
                WrapperResult::Continue
            }
            Ok(session) => match session.0.read().await.data.get::<RequireUpdate>() {
                None => WrapperResult::Continue,
                Some(update_required) => {
                    if update_required.0.load(Ordering::Relaxed)
                        && !data.request.path.matches("/account")
                    {
                        *data.response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
                        data.response.headers_mut().insert(
                            HeaderName::from_static("location"),
                            HeaderValue::from_static("/user"),
                        );
                        WrapperResult::Return
                    } else {
                        WrapperResult::Continue
                    }
                }
            },
        }
    }

    async fn after(&self, _: &mut ServiceData) -> WrapperResult {
        WrapperResult::Continue
    }
}
