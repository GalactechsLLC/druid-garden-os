use crate::database::users::{
    self, UserPasswordUpdate, UserWithInfoWithPassword, UsernameWithPassword,
};
use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use portfu::prelude::async_trait::async_trait;
use portfu::prelude::http::StatusCode;
use portfu::prelude::{Path, State};
use portfu::wrappers::sessions::Session;
use portfu_admin::auth::{BasicAuth, Claims};
use portfu_admin::users::UserRole;
use portfu_core::wrappers::{WrapperFn, WrapperResult};
use portfu_core::{FromRequest, Json, ServiceData};
use portfu_macros::{get, post, put};
use serde::{Deserialize, Serialize};
use sqlx::types::time::OffsetDateTime;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const MAX_LOGIN_ACCOUNTS: usize = 1024;
#[derive(Default)]
struct LoginThrottle {
    global: Option<(Instant, u32)>,
    accounts: HashMap<String, (Instant, u32)>,
}
impl LoginThrottle {
    fn admit(&mut self, username: &str, now: Instant) -> bool {
        self.accounts
            .retain(|_, (start, _)| now.duration_since(*start) < LOGIN_WINDOW);
        let global = self.global.get_or_insert((now, 0));
        if now.duration_since(global.0) >= LOGIN_WINDOW {
            *global = (now, 0);
        }
        if global.1 >= 60 {
            return false;
        }
        if !self.accounts.contains_key(username) && self.accounts.len() >= MAX_LOGIN_ACCOUNTS {
            return false;
        }
        let account = self
            .accounts
            .entry(username.to_string())
            .or_insert((now, 0));
        if account.1 >= 8 {
            return false;
        }
        global.1 += 1;
        account.1 += 1;
        true
    }
}
static LOGIN_THROTTLE: LazyLock<Mutex<LoginThrottle>> = LazyLock::new(Default::default);
fn admit_password_attempt(username: &str, password: &[u8]) -> Result<(), Error> {
    users::validate_username(username)?;
    if password.len() > 1024 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Password exceeds limit",
        ));
    }
    if !LOGIN_THROTTLE
        .lock()
        .map_err(|_| Error::other("Login limiter unavailable"))?
        .admit(username, Instant::now())
    {
        return Err(Error::new(
            ErrorKind::WouldBlock,
            "Too many password attempts; retry in one minute",
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct SessionGeneration(i64);
#[derive(Clone)]
struct ValidatedAccount(Claims);

pub struct BasicAuthHandle {
    pool: SqlitePool,
    argon: Argon2<'static>,
    dummy_hash: Vec<u8>,
}
impl BasicAuthHandle {
    pub fn new(pool: SqlitePool, argon: Argon2<'static>) -> Result<Self, Error> {
        let dummy_hash = argon
            .hash_password(
                b"nonexistent-account-placeholder",
                &SaltString::generate(&mut OsRng),
            )
            .map_err(|error| Error::other(error.to_string()))?
            .to_string()
            .into_bytes();
        Ok(Self {
            pool,
            argon,
            dummy_hash,
        })
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
        let username = username.as_ref();
        let password = password.as_ref();
        admit_password_attempt(username, password.as_bytes())?;
        let record = users::authentication_record(&self.pool, username).await?;
        let encoded = record
            .as_ref()
            .map_or(&self.dummy_hash, |record| &record.user.password)
            .clone();
        let valid =
            users::verify_password(self.argon.clone(), password.as_bytes().to_vec(), encoded)
                .await?;
        let record = record
            .filter(|record| valid && record.user.role >= UserRole::User)
            .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Invalid credentials"))?;
        let must_change = password == "Admin"
            || (username == "Admin"
                && crate::first_run::bootstrap_password(false)?
                    .is_some_and(|bootstrap| bootstrap == password.as_bytes()));
        let now = OffsetDateTime::now_utc().unix_timestamp().max(0) as usize;
        let claims = Claims {
            aud: "localhost".into(),
            exp: now + 30 * 60,
            iat: now,
            iss: "localhost".into(),
            nbf: now,
            sub: record.user.id.to_string(),
            eml: record.user.username,
            rol: record.user.role,
            org: vec![],
        };
        let mut session = session.write().await;
        session
            .data
            .insert(RequireUpdate(Arc::new(AtomicBool::new(must_change))));
        session.data.insert(SessionGeneration(record.generation));
        session.data.insert(claims.clone());
        Ok(claims)
    }
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

#[post("/auth/login", output = "json", eoutput = "bytes")]
pub async fn login_endpoint(
    handle: State<BasicAuthHandle>,
    session: State<RwLock<Session>>,
    payload: Json<Option<LoginRequest>>,
    data: &mut ServiceData,
) -> Result<String, Error> {
    let Some(payload) = payload.inner() else {
        *data.response.status_mut() = StatusCode::BAD_REQUEST;
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "A JSON login request is required",
        ));
    };
    if let Err(error) = handle
        .login(payload.username, payload.password, session.0.clone())
        .await
    {
        *data.response.status_mut() = match error.kind() {
            ErrorKind::WouldBlock => {
                data.response.headers_mut().insert(
                    "retry-after",
                    portfu::prelude::http::HeaderValue::from_static("60"),
                );
                StatusCode::TOO_MANY_REQUESTS
            }
            ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            ErrorKind::PermissionDenied => StatusCode::UNAUTHORIZED,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        return Err(error);
    }
    portfu_admin::auth::get_jwt::get_jwt(data).await
}

#[post("/api/users/register", output = "json", eoutput = "bytes")]
pub async fn register_endpoint(
    pool: State<SqlitePool>,
    argon: State<Argon2<'static>>,
    data: Json<Option<UsernameWithPassword>>,
) -> Result<bool, Error> {
    let data = data
        .inner()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Invalid account"))?;
    users::register(
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
    .map(|user| user.is_some())
}

#[put("/api/users/password", output = "json", eoutput = "bytes")]
pub async fn user_update_password(
    pool: State<SqlitePool>,
    argon: State<Argon2<'static>>,
    session: State<RwLock<Session>>,
    data: Json<Option<UserPasswordUpdate>>,
) -> Result<bool, Error> {
    let data = data
        .inner()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Invalid password update"))?;
    let claims = session
        .0
        .read()
        .await
        .data
        .get::<Claims>()
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Authentication required"))?;
    if data.username != claims.eml {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Cannot change another user's password",
        ));
    }
    admit_password_attempt(&data.username, data.old_password.as_bytes())?;
    users::validate_new_password(data.new_password.as_bytes())?;
    let reuses_bootstrap = data.username == "Admin"
        && crate::first_run::bootstrap_password(false)?
            .is_some_and(|bootstrap| bootstrap == data.new_password.as_bytes());
    if data.new_password == data.old_password || reuses_bootstrap {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Choose a new non-bootstrap password",
        ));
    }
    let generation = session
        .0
        .read()
        .await
        .data
        .get::<SessionGeneration>()
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Session revoked"))?
        .0;
    let result = users::update_password(pool.as_ref(), argon.0.clone(), data, generation).await?;
    // The database generation invalidates every other session on its next request.
    let mut session = session.0.write().await;
    session.data.remove::<Claims>();
    session.data.remove::<SessionGeneration>();
    session.data.remove::<RequireUpdate>();
    Ok(result)
}

#[get("/api/users/password/{username}", output = "json", eoutput = "bytes")]
pub async fn user_requires_password_update(
    session: State<RwLock<Session>>,
    username: Path,
) -> Result<bool, Error> {
    let session = session.0.read().await;
    let claims = session
        .data
        .get::<Claims>()
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Authentication required"))?;
    if username.inner() != claims.eml {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "Cannot inspect another account",
        ));
    }
    Ok(session
        .data
        .get::<RequireUpdate>()
        .is_none_or(|required| required.0.load(Ordering::Relaxed)))
}

#[derive(Serialize)]
pub struct CurrentAccount {
    id: String,
    expires_at: usize,
    username: String,
    role: UserRole,
    must_change_password: bool,
}
#[get("/api/users/me", output = "json", eoutput = "bytes")]
pub async fn current_account(session: State<RwLock<Session>>) -> Result<CurrentAccount, Error> {
    let session = session.0.read().await;
    let claims = session
        .data
        .get::<Claims>()
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Authentication required"))?;
    Ok(CurrentAccount {
        id: claims.sub.clone(),
        expires_at: claims.exp,
        username: claims.eml.clone(),
        role: claims.rol,
        must_change_password: session
            .data
            .get::<RequireUpdate>()
            .is_none_or(|required| required.0.load(Ordering::Relaxed)),
    })
}
#[get("/api/users", output = "json", eoutput = "bytes")]
pub async fn list_accounts(pool: State<SqlitePool>) -> Result<Vec<users::AccountSummary>, Error> {
    users::list_accounts(&pool.0).await
}
#[derive(Deserialize)]
pub struct RoleUpdate {
    role: UserRole,
}
#[put("/api/users/{id}/role", output = "json", eoutput = "bytes")]
pub async fn update_account_role(
    pool: State<SqlitePool>,
    session: State<RwLock<Session>>,
    id: Path,
    payload: Json<Option<RoleUpdate>>,
) -> Result<bool, Error> {
    let id: i64 = id
        .inner()
        .parse()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "Invalid account ID"))?;
    let role = payload
        .inner()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Missing role"))?
        .role;
    let (actor_id, generation) = {
        let session = session.0.read().await;
        let claims = session
            .data
            .get::<Claims>()
            .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Authentication required"))?;
        let generation = session
            .data
            .get::<SessionGeneration>()
            .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Session revoked"))?
            .0;
        (
            claims
                .sub
                .parse()
                .map_err(|_| Error::new(ErrorKind::PermissionDenied, "Invalid account"))?,
            generation,
        )
    };
    users::change_role(&pool.0, actor_id, generation, id, role).await
}

#[derive(Clone)]
pub struct RequireUpdate(pub Arc<AtomicBool>);

fn authorize_claims(claims: &Claims, required: UserRole, now: usize) -> Result<(), StatusCode> {
    if claims.exp <= now || claims.nbf > now || claims.sub.is_empty() {
        Err(StatusCode::UNAUTHORIZED)
    } else if claims.rol < required {
        Err(StatusCode::FORBIDDEN)
    } else {
        Ok(())
    }
}

#[post("/auth/logout", output = "json", eoutput = "bytes")]
pub async fn logout(session: State<RwLock<Session>>) -> Result<bool, Error> {
    let mut session = session.0.write().await;
    session.data.remove::<Claims>();
    session.data.remove::<RequireUpdate>();
    session.data.remove::<SessionGeneration>();
    Ok(true)
}

/// Enforces authenticated, unexpired sessions independently of UI navigation.
pub struct RequireRole(pub UserRole);

#[async_trait]
impl WrapperFn for RequireRole {
    fn name(&self) -> &str {
        "RequireRole"
    }

    async fn before(&self, data: &mut ServiceData) -> WrapperResult {
        let claims = if let Some(account) = data.request.get::<ValidatedAccount>() {
            Some(account.0.clone())
        } else {
            let session = data.request.get::<Arc<RwLock<Session>>>().cloned();
            let snapshot = if let Some(session) = &session {
                let session = session.read().await;
                session.data.get::<Claims>().cloned().zip(
                    session
                        .data
                        .get::<SessionGeneration>()
                        .map(|generation| generation.0),
                )
            } else {
                None
            };
            if let Some((claims, generation)) = snapshot {
                let pool = match State::<SqlitePool>::from_request(&mut data.request, "pool").await
                {
                    Ok(pool) => pool,
                    Err(_) => {
                        *data.response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        return WrapperResult::Return;
                    }
                };
                match users::authentication_record(&pool.0, &claims.eml).await {
                    Ok(Some(record))
                        if record.user.id.to_string() == claims.sub
                            && record.generation == generation
                            && record.user.role == claims.rol =>
                    {
                        data.request.insert(ValidatedAccount(claims.clone()));
                        Some(claims)
                    }
                    Ok(_) => {
                        if let Some(session) = session {
                            let mut session = session.write().await;
                            session.data.remove::<Claims>();
                            session.data.remove::<SessionGeneration>();
                            session.data.remove::<RequireUpdate>();
                        }
                        None
                    }
                    Err(_) => {
                        *data.response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                        return WrapperResult::Return;
                    }
                }
            } else {
                None
            }
        };
        let now = OffsetDateTime::now_utc().unix_timestamp().max(0) as usize;
        match claims
            .as_ref()
            .map(|claims| authorize_claims(claims, self.0, now))
        {
            Some(Ok(())) => WrapperResult::Continue,
            result => {
                *data.response.status_mut() = result
                    .and_then(Result::err)
                    .unwrap_or(StatusCode::UNAUTHORIZED);
                WrapperResult::Return
            }
        }
    }

    async fn after(&self, _: &mut ServiceData) -> WrapperResult {
        WrapperResult::Continue
    }
}

/// A long-lived connection keeps its original account and expiry; signing in
/// again on the same cookie cannot silently transfer or extend that connection.
#[derive(Clone)]
pub(crate) struct SessionLease {
    claims: Claims,
    generation: i64,
}
impl SessionLease {
    pub(crate) async fn capture(session: &RwLock<Session>) -> Result<Self, Error> {
        let session = session.read().await;
        Ok(Self {
            claims: session.data.get::<Claims>().cloned().ok_or_else(|| {
                Error::new(ErrorKind::PermissionDenied, "Authentication required")
            })?,
            generation: session
                .data
                .get::<SessionGeneration>()
                .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "Session revoked"))?
                .0,
        })
    }
    async fn valid(
        &self,
        pool: &SqlitePool,
        session: &RwLock<Session>,
        required: UserRole,
    ) -> Result<bool, Error> {
        let now = OffsetDateTime::now_utc().unix_timestamp().max(0) as usize;
        if authorize_claims(&self.claims, required, now).is_err() {
            return Ok(false);
        }
        {
            let session = session.read().await;
            let unchanged = session.data.get::<Claims>().is_some_and(|current| {
                current.sub == self.claims.sub
                    && current.exp == self.claims.exp
                    && current.rol == self.claims.rol
            }) && session
                .data
                .get::<SessionGeneration>()
                .is_some_and(|current| current.0 == self.generation)
                && session
                    .data
                    .get::<RequireUpdate>()
                    .is_some_and(|required| !required.0.load(Ordering::Relaxed));
            if !unchanged {
                return Ok(false);
            }
        }
        Ok(users::authentication_record(pool, &self.claims.eml)
            .await?
            .is_some_and(|record| {
                record.user.id.to_string() == self.claims.sub
                    && record.generation == self.generation
                    && record.user.role == self.claims.rol
            }))
    }
    pub(crate) async fn wait_until_revoked(
        &self,
        pool: &SqlitePool,
        session: &RwLock<Session>,
        required: UserRole,
    ) -> Result<(), Error> {
        loop {
            match tokio::time::timeout(Duration::from_secs(2), self.valid(pool, session, required))
                .await
            {
                Ok(Ok(true)) => tokio::time::sleep(Duration::from_secs(3)).await,
                _ => {
                    return Err(Error::new(
                        ErrorKind::PermissionDenied,
                        "Log session expired or was revoked",
                    ))
                }
            }
        }
    }
}

pub struct PasswordUpdateWrapper {}
#[async_trait]
impl WrapperFn for PasswordUpdateWrapper {
    fn name(&self) -> &str {
        "PasswordUpdateWrapper"
    }

    async fn before(&self, data: &mut ServiceData) -> WrapperResult {
        let session = data.request.get::<Arc<RwLock<Session>>>().cloned();
        if let Some(session) = session {
            let session = session.read().await;
            if let Some(update_required) = session.data.get::<RequireUpdate>() {
                if !update_required.0.load(Ordering::Relaxed) {
                    return WrapperResult::Continue;
                }
            }
        }
        // Password endpoints are registered before this wrapper. API calls must
        // fail closed instead of redirecting a mutation to the SPA.
        *data.response.status_mut() = StatusCode::FORBIDDEN;
        WrapperResult::Return
    }

    async fn after(&self, _: &mut ServiceData) -> WrapperResult {
        WrapperResult::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(role: UserRole) -> Claims {
        Claims {
            sub: "1".to_string(),
            exp: 200,
            nbf: 100,
            rol: role,
            ..Claims::default()
        }
    }

    #[tokio::test]
    async fn active_connection_lease_rejects_revocation_and_cookie_replacement() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT, password BLOB, role TEXT, auth_generation INTEGER)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO users VALUES (1, 'viewer', x'00', 'Viewer', 0)")
            .execute(&pool)
            .await
            .unwrap();
        let now = OffsetDateTime::now_utc().unix_timestamp() as usize;
        let mut extensions = portfu::prelude::http::Extensions::new();
        let mut identity = claims(UserRole::Viewer);
        identity.eml = "viewer".into();
        identity.nbf = now;
        identity.exp = now + 600;
        extensions.insert(identity.clone());
        extensions.insert(SessionGeneration(0));
        extensions.insert(RequireUpdate(Arc::new(AtomicBool::new(false))));
        let session = RwLock::new(Session {
            data: extensions,
            last_update: Instant::now(),
        });
        let lease = SessionLease::capture(&session).await.unwrap();
        assert!(lease
            .valid(&pool, &session, UserRole::Viewer)
            .await
            .unwrap());
        sqlx::query("UPDATE users SET auth_generation=1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!lease
            .valid(&pool, &session, UserRole::Viewer)
            .await
            .unwrap());
        sqlx::query("UPDATE users SET auth_generation=0")
            .execute(&pool)
            .await
            .unwrap();
        identity.exp += 600;
        session.write().await.data.insert(identity);
        assert!(!lease
            .valid(&pool, &session, UserRole::Viewer)
            .await
            .unwrap());
        session.write().await.data.remove::<Claims>();
        assert!(!lease
            .valid(&pool, &session, UserRole::Viewer)
            .await
            .unwrap());
    }

    #[test]
    fn login_throttle_is_bounded_and_expires() {
        let mut throttle = LoginThrottle::default();
        let now = Instant::now();
        for _ in 0..8 {
            assert!(throttle.admit("owner", now));
        }
        assert!(!throttle.admit("owner", now));
        for i in 0..52 {
            assert!(throttle.admit(&format!("other-{i}"), now));
        }
        assert!(!throttle.admit("new-account", now));
        assert!(throttle.admit("owner", now + LOGIN_WINDOW));
        assert_eq!(throttle.accounts.len(), 1);
    }

    #[test]
    fn sessions_must_be_current_and_identified() {
        let mut session = claims(UserRole::SuperAdmin);
        assert_eq!(
            authorize_claims(&session, UserRole::User, 99),
            Err(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            authorize_claims(&session, UserRole::User, 200),
            Err(StatusCode::UNAUTHORIZED)
        );
        session.sub.clear();
        assert_eq!(
            authorize_claims(&session, UserRole::User, 150),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    #[test]
    fn viewer_cannot_manage_device() {
        assert_eq!(
            authorize_claims(&claims(UserRole::Viewer), UserRole::Manager, 150),
            Err(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            authorize_claims(&claims(UserRole::Manager), UserRole::Manager, 150),
            Ok(())
        );
        assert_eq!(
            authorize_claims(&claims(UserRole::SuperAdmin), UserRole::Editor, 150),
            Ok(())
        );
    }
}
