use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    input::{InputEvent, InputState},
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::host::{CredentialId, CredentialKind, HostStore};
use crate::i18n::t;

#[derive(Clone)]
struct PasswordChoice {
    id: CredentialId,
    name: SharedString,
}

impl SearchableListItem for PasswordChoice {
    type Value = CredentialId;

    fn title(&self) -> SharedString {
        self.name.clone()
    }

    fn value(&self) -> &CredentialId {
        &self.id
    }
}

pub(crate) struct SavedPasswordPicker {
    store: Entity<HostStore>,
    input: Entity<InputState>,
    choice: Entity<SelectState<Vec<PasswordChoice>>>,
    revision: u64,
    loading: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SavedPasswordPicker {
    pub(crate) fn new(
        store: Entity<HostStore>,
        input: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choices = store
            .read(cx)
            .credentials()
            .iter()
            .filter(|credential| credential.kind == CredentialKind::Password)
            .map(|credential| PasswordChoice {
                id: credential.id,
                name: credential.name.clone(),
            })
            .collect::<Vec<_>>();
        let choice = cx.new(|cx| SelectState::new(choices, None, window, cx).searchable(true));
        let subscriptions = vec![
            cx.subscribe_in(&choice, window, |this, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(id)) = event {
                    this.choose(*id, window, cx);
                }
            }),
            cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.cancel();
                    this.choice
                        .update(cx, |choice, cx| choice.set_selected_index(None, window, cx));
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            input,
            choice,
            revision: 0,
            loading: false,
            error: None,
            task: None,
            _subscriptions: subscriptions,
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.revision += 1;
        self.task = None;
        self.loading = false;
        self.error = None;
    }

    pub(crate) fn problem(&self) -> Option<SharedString> {
        if self.loading {
            Some(t!("credential.password_picker.loading"))
        } else {
            self.error.clone()
        }
    }

    fn choose(&mut self, id: CredentialId, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel();
        let Some(credential) = self
            .store
            .read(cx)
            .credential(id)
            .filter(|credential| credential.kind == CredentialKind::Password)
        else {
            self.error = Some(t!("credential.password_picker.unavailable"));
            cx.notify();
            return;
        };
        let secret = credential.password_secret();
        let secrets = self.store.read(cx).secrets();
        let revision = self.revision;
        self.loading = true;
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { secrets.get(&secret) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.revision != revision {
                    return;
                }
                this.loading = false;
                if this
                    .store
                    .read(cx)
                    .credential(id)
                    .is_none_or(|credential| credential.kind != CredentialKind::Password)
                {
                    this.error = Some(t!("credential.password_picker.unavailable"));
                } else {
                    match result {
                        Ok(Some(password)) => this.input.update(cx, |input, cx| {
                            input.set_value(password.as_str(), window, cx)
                        }),
                        Ok(None) | Err(_) => {
                            this.error = Some(t!("credential.password_picker.unavailable"))
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for SavedPasswordPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("saved-password-control")
            .gap_1()
            .w_full()
            .min_w_0()
            .tooltip(|window, cx| {
                Tooltip::new(t!("credential.password_picker.hint")).build(window, cx)
            })
            .child(
                Select::new(&self.choice)
                    .id("saved-password-picker")
                    .small()
                    .placeholder(t!("credential.password_picker.select"))
                    .accessibility_label(t!("credential.password_picker.select")),
            )
            .when(self.loading, |view| {
                view.child(
                    div()
                        .id("saved-password-loading")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("credential.password_picker.loading")),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("saved-password-error")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
