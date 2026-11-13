use std::{path::Path, sync::Arc};

use gpui_kit::*;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalBackground(Option<BackgroundImage>);

impl Global for TerminalBackground {}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundImage {
    image: Arc<Path>,
    opacity: f32,
}

impl BackgroundImage {
    pub fn new(image: impl Into<Arc<Path>>, opacity: f32) -> Self {
        Self {
            image: image.into(),
            opacity: opacity.clamp(0., 1.),
        }
    }

    pub fn image(&self) -> &Arc<Path> {
        &self.image
    }
    pub fn opacity(&self) -> f32 {
        self.opacity
    }
    pub fn veil(&self, background: Hsla) -> Hsla {
        background.opacity(1. - self.opacity)
    }
}

impl TerminalBackground {
    pub fn new(image: Option<BackgroundImage>) -> Self {
        Self(image)
    }

    pub fn current(cx: &App) -> Option<&BackgroundImage> {
        cx.try_global::<Self>()?.0.as_ref()
    }

    pub fn layers(background: &BackgroundImage, color: Hsla) -> [AnyElement; 2] {
        [
            img(background.image.clone())
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            div()
                .absolute()
                .inset_0()
                .bg(background.veil(color))
                .into_any_element(),
        ]
    }
}
