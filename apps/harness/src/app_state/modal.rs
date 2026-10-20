use crate::providers::{ProviderOption, SessionSelection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    Commands {
        selected: usize,
    },
    Provider {
        selected: usize,
    },
    Account {
        provider: usize,
    },
    Model {
        provider: usize,
        selected: usize,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalResult {
    None,
    StartSession(SessionSelection),
}

pub(super) fn confirm_account(
    provider: usize,
    providers: &[ProviderOption],
) -> Result<ModalState, String> {
    let option = &providers[provider.min(providers.len() - 1)];
    if !option.ready {
        return Err(option.health_message.clone());
    }

    let selected = option
        .models
        .iter()
        .position(|model| model == &option.default_model)
        .unwrap_or(0);
    Ok(ModalState::Model {
        provider,
        selected,
        value: option.default_model.clone(),
    })
}

pub(super) fn confirm_model(
    provider: usize,
    value: String,
    providers: &[ProviderOption],
) -> Result<SessionSelection, (ModalState, String)> {
    let provider_index = provider.min(providers.len() - 1);
    let provider_option = &providers[provider_index];
    let model = value.trim();
    if model.is_empty() {
        return Err((
            ModalState::Model {
                provider: provider_index,
                selected: usize::MAX,
                value,
            },
            "Model ID cannot be empty".to_owned(),
        ));
    }

    Ok(SessionSelection {
        provider_id: provider_option.id.clone(),
        credential_profile: provider_option.credential_profile.clone(),
        integration: provider_option.integration.clone(),
        provider: provider_option.name.clone(),
        model: model.to_owned(),
    })
}
