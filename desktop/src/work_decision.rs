//! Per-profile typed-decision preference. The stored choice is trusted host
//! state; TypeSafe/Jev secrets cross settings only inbound and use the existing
//! fixed native credential item. Settings never return stored secret content.
use super::*;
use zephium_core::ids::ProfileId;
use zephium_core::work::WorkError;
use zephium_ipc::work::{
    WorkDecisionChoiceV1, WorkDecisionPreferenceChangedV1, WorkDecisionPreferenceV1,
};

const SETTING_PREFIX: &str = "work.decisions.";

fn log(arguments: std::fmt::Arguments<'_>) {
    #[cfg(feature = "work-product")]
    super::work_provider::record_diagnostic(arguments);
    #[cfg(not(feature = "work-product"))]
    super::write_diagnostic(arguments);
}

fn setting_key(profile: ProfileId) -> String {
    format!("{SETTING_PREFIX}{profile}")
}

/// 0 unobserved, 1 absent, 2 present. Process-wide: the item is fixed, not
/// per-profile, so one observation serves every profile.
static TYPESAFE_PRESENCE: AtomicU8 = AtomicU8::new(0);
static TYPESAFE_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PRESENCE_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

type Observer = Box<dyn Fn(WorkDecisionPreferenceChangedV1) + Send + Sync>;
static OBSERVER: OnceLock<Observer> = OnceLock::new();

pub(crate) fn set_observer(observer: Observer) {
    let _ = OBSERVER.set(observer);
}

fn notify(profile: ProfileId) {
    if let Some(observer) = OBSERVER.get() {
        observer(WorkDecisionPreferenceChangedV1 {
            profile: profile.to_string(),
        });
    }
}

/// Records what the existing loader just saw. Only a change from an already
/// observed state is an invalidation; the first observation is not.
fn record_presence(profile: ProfileId, present: bool) {
    let next = if present { 2 } else { 1 };
    let previous = TYPESAFE_PRESENCE.swap(next, Ordering::AcqRel);
    if previous != 0 && previous != next {
        notify(profile);
    }
}

#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn load_presence() -> bool {
    // Exactly what a run treats as usable: anything else already falls back.
    zephium_agentic::load_development_typesafe_credential().is_ok()
}

#[cfg(not(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
)))]
fn load_presence() -> bool {
    false
}

/// The presence the TypeSafe key had when it was last loaded. A settings read
/// observes it once per mutation epoch; later answers reflect acknowledged
/// key changes or current-epoch provider construction.
async fn typesafe_key_present(profile: ProfileId) -> bool {
    match TYPESAFE_PRESENCE.load(Ordering::Acquire) {
        2 => return true,
        1 => return false,
        _ => {}
    }
    let epoch = TYPESAFE_EPOCH.load(Ordering::Acquire);
    let Ok(present) = tokio::task::spawn_blocking(load_presence).await else {
        return false;
    };
    let _turn = PRESENCE_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if TYPESAFE_EPOCH.load(Ordering::Acquire) == epoch {
        record_presence(profile, present);
        present
    } else {
        TYPESAFE_PRESENCE.load(Ordering::Acquire) == 2
    }
}

/// Observation from the one site that already loads the key for a real run.
#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
pub(crate) fn observe_typesafe_key(profile: ProfileId, epoch: u64, present: bool) {
    let _turn = PRESENCE_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if TYPESAFE_EPOCH.load(Ordering::Acquire) == epoch {
        record_presence(profile, present);
    }
}

#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
pub(crate) fn typesafe_presence_epoch() -> u64 {
    TYPESAFE_EPOCH.load(Ordering::Acquire)
}

#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn begin_presence_mutation() {
    let _presence = PRESENCE_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    TYPESAFE_EPOCH.fetch_add(1, Ordering::AcqRel);
    TYPESAFE_PRESENCE.store(0, Ordering::Release);
}

#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
fn finish_presence_mutation(profile: ProfileId, present: Option<bool>) {
    let _presence = PRESENCE_TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Fence loads that began before or during the vault mutation.
    TYPESAFE_EPOCH.fetch_add(1, Ordering::AcqRel);
    TYPESAFE_PRESENCE.store(0, Ordering::Release);
    if let Some(present) = present {
        record_presence(profile, present);
    }
}

async fn read_choice(profile: ProfileId) -> WorkDecisionChoiceV1 {
    let key = setting_key(profile);
    tokio::task::spawn_blocking(move || {
        use zephium_core::ports::store::Store;
        APP_STORE
            .get()
            .and_then(|store| store.app_setting(&key))
            .as_deref()
            .and_then(WorkDecisionChoiceV1::parse)
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

async fn write_choice(profile: ProfileId, choice: WorkDecisionChoiceV1) -> Result<(), WorkError> {
    let key = setting_key(profile);
    let stored = tokio::task::spawn_blocking(move || {
        use zephium_core::ports::store::Store;
        APP_STORE
            .get()
            .is_some_and(|store| store.set_app_setting(key, choice.as_str().to_owned()))
    })
    .await
    .map_err(|_| WorkError::Unavailable)?;
    if stored {
        Ok(())
    } else {
        Err(WorkError::Capacity)
    }
}

/// The preference the next read will run under; a running read keeps the
/// configuration it was constructed with.
#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
pub(crate) async fn selected_choice(profile: ProfileId) -> WorkDecisionChoiceV1 {
    read_choice(profile).await
}

#[cfg(all(
    feature = "work-product",
    any(target_os = "macos", target_os = "windows", target_os = "linux")
))]
pub(crate) fn composition_preference(
    choice: WorkDecisionChoiceV1,
) -> zephium_work_composition::durable_runtime::WorkDecisionPreference {
    use zephium_work_composition::durable_runtime::WorkDecisionPreference;
    match choice {
        WorkDecisionChoiceV1::Recommended => WorkDecisionPreference::Recommended,
        WorkDecisionChoiceV1::Standard => WorkDecisionPreference::Emulation,
        WorkDecisionChoiceV1::Off => WorkDecisionPreference::Disabled,
    }
}

/// The actor selects the Work profile; a caller-supplied id may only confirm it.
async fn confirm_profile(app: &tauri::AppHandle, expected: &str) -> Result<ProfileId, WorkError> {
    if shutdown_started(app) {
        return Err(WorkError::Shutdown);
    }
    let profile = ProfileId::parse(expected)
        .filter(|id| id.to_string() == expected)
        .ok_or(WorkError::Invalid)?;
    #[cfg(all(
        feature = "work-product",
        any(target_os = "macos", target_os = "windows", target_os = "linux")
    ))]
    {
        let request = app.state::<zephium_app::Handle>().work_profile_binding();
        let readiness = tokio::time::timeout(std::time::Duration::from_secs(4), async move {
            loop {
                if let Some(readiness) = request.try_recv() {
                    break readiness;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| WorkError::Unavailable)?;
        let bound = match readiness {
            zephium_app::AgentWorkProfileReadiness::Ready(binding)
            | zephium_app::AgentWorkProfileReadiness::PolicyPending(binding) => binding.profile(),
            _ => return Err(WorkError::ProfileUnavailable),
        };
        if bound != profile {
            return Err(WorkError::ProfileUnavailable);
        }
        Ok(profile)
    }
    #[cfg(not(all(
        feature = "work-product",
        any(target_os = "macos", target_os = "windows", target_os = "linux")
    )))]
    {
        let _ = profile;
        Err(WorkError::Unavailable)
    }
}

async fn preference(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
    selection: Option<WorkDecisionChoiceV1>,
    label: &'static str,
) -> WorkDecisionPreferenceV1 {
    let result = async {
        if !authorize(&caller, CallerPolicy::Main, label) {
            return Err(WorkError::Unavailable);
        }
        let profile = confirm_profile(&app, &expected_profile).await?;
        if let Some(choice) = selection {
            write_choice(profile, choice).await?;
            notify(profile);
        }
        let choice = read_choice(profile).await;
        let present = typesafe_key_present(profile).await;
        log(format_args!(
            "work: phase=decision_preference choice={} effective={} typesafe_key={present}",
            choice.as_str(),
            choice.effective(present).as_str(),
        ));
        Ok((choice, present))
    }
    .await;
    match result {
        Ok((choice, present)) => {
            WorkDecisionPreferenceV1::settled(expected_profile, choice, present)
        }
        Err(error) => WorkDecisionPreferenceV1::failed(expected_profile, error.into()),
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn work_decision_preference(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
) -> WorkDecisionPreferenceV1 {
    preference(
        caller,
        app,
        expected_profile,
        None,
        "work_decision_preference",
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn work_set_decision_preference(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
    choice: WorkDecisionChoiceV1,
) -> WorkDecisionPreferenceV1 {
    preference(
        caller,
        app,
        expected_profile,
        Some(choice),
        "work_set_decision_preference",
    )
    .await
}

async fn mutate_key(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
    secret: Option<String>,
    label: &'static str,
) -> WorkDecisionPreferenceV1 {
    let result: Result<_, WorkError> = async {
        if !authorize(&caller, CallerPolicy::Main, label) {
            return Err(WorkError::Unavailable);
        }
        #[cfg(all(
            feature = "work-product",
            any(target_os = "macos", target_os = "windows", target_os = "linux")
        ))]
        {
            use zephium_agentic::lead::{keys, LeadSecret};
            // Own and validate the inbound secret before the first await. It
            // cannot appear in a response, diagnostic, or stored preference.
            let secret = secret
                .map(LeadSecret::new)
                .transpose()
                .map_err(|_| WorkError::Invalid)?;
            let profile = confirm_profile(&app, &expected_profile).await?;
            static MUTATIONS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _turn = MUTATIONS.lock().await;
            confirm_profile(&app, &expected_profile).await?;
            let present = secret.is_some();
            begin_presence_mutation();
            let stored = tokio::task::spawn_blocking(move || match secret {
                Some(secret) => keys::store_typesafe(secret),
                None => keys::clear_typesafe(),
            })
            .await
            .map_err(|_| WorkError::OutcomeUnknown)
            .and_then(|result| {
                result.map_err(|error| match error {
                    keys::LeadKeyError::Invalid => WorkError::Invalid,
                    keys::LeadKeyError::Missing
                    | keys::LeadKeyError::Inaccessible
                    | keys::LeadKeyError::AwaitingApproval => WorkError::Unavailable,
                })
            });
            finish_presence_mutation(profile, stored.is_ok().then_some(present));
            // The key is global, while the choice remains profile-scoped.
            // Acknowledged mutation invalidates observers even after first load.
            notify(profile);
            stored?;
            Ok((read_choice(profile).await, present))
        }
        #[cfg(not(all(
            feature = "work-product",
            any(target_os = "macos", target_os = "windows", target_os = "linux")
        )))]
        {
            let _ = (app, secret);
            Err(WorkError::Unavailable)
        }
    }
    .await;
    match result {
        Ok((choice, present)) => {
            WorkDecisionPreferenceV1::settled(expected_profile, choice, present)
        }
        Err(error) => WorkDecisionPreferenceV1::failed(expected_profile, error.into()),
    }
}

/// Inbound-only Jev key setup, authorized against the current regular profile.
#[tauri::command]
#[specta::specta]
pub(crate) async fn work_set_decision_key(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
    secret: String,
) -> WorkDecisionPreferenceV1 {
    mutate_key(
        caller,
        app,
        expected_profile,
        Some(secret),
        "work_set_decision_key",
    )
    .await
}

/// Removes the separate native decision key without reading it back.
#[tauri::command]
#[specta::specta]
pub(crate) async fn work_clear_decision_key(
    caller: WebviewWindow,
    app: tauri::AppHandle,
    expected_profile: String,
) -> WorkDecisionPreferenceV1 {
    mutate_key(
        caller,
        app,
        expected_profile,
        None,
        "work_clear_decision_key",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_key_is_profile_scoped_and_carries_the_wire_spelling() {
        let profile = ProfileId::from(7_u128);
        assert_eq!(setting_key(profile), format!("work.decisions.{profile}"));
        assert_eq!(WorkDecisionChoiceV1::Standard.as_str(), "standard");
    }

    #[cfg(all(
        feature = "work-product",
        any(target_os = "macos", target_os = "windows", target_os = "linux")
    ))]
    #[test]
    fn late_provider_observations_cannot_override_acknowledged_key_mutations() {
        static TEST_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _test = TEST_TURN.lock().expect("test presence owner");
        struct Restore(u64, u8);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _turn = PRESENCE_TURN
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                TYPESAFE_EPOCH.store(self.0, Ordering::Release);
                TYPESAFE_PRESENCE.store(self.1, Ordering::Release);
            }
        }
        let _restore = {
            let _turn = PRESENCE_TURN.lock().expect("presence owner");
            Restore(
                TYPESAFE_EPOCH.load(Ordering::Acquire),
                TYPESAFE_PRESENCE.load(Ordering::Acquire),
            )
        };
        let profile = ProfileId::from(7_u128);
        for (acknowledged, late_result, expected) in [(true, false, 2), (false, true, 1)] {
            let stale = typesafe_presence_epoch();
            begin_presence_mutation();
            // Same publication used after successful native save/removal, with
            // no vault access or real key needed to prove the callback race.
            finish_presence_mutation(profile, Some(acknowledged));
            observe_typesafe_key(profile, stale, late_result);
            assert_eq!(TYPESAFE_PRESENCE.load(Ordering::Acquire), expected);
            // Loads admitted during the write are stale at acknowledgement too.
            observe_typesafe_key(profile, typesafe_presence_epoch() - 1, late_result);
            assert_eq!(TYPESAFE_PRESENCE.load(Ordering::Acquire), expected);
        }
    }
}
