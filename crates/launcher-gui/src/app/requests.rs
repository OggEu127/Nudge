//! Read-request transitions, independent of painting and async execution.

use crate::jobs::Action;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum RequestState {
    #[default]
    Idle,
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Copy)]
enum ReadRequest {
    Versions,
    Content,
}

#[derive(Default)]
pub(super) struct ReadRequests {
    pub(super) versions: RequestState,
    pub(super) content: RequestState,
    active: Option<ReadRequest>,
}

impl ReadRequests {
    pub(super) fn begin(&mut self, action: &Action) {
        self.active = match action {
            Action::Versions => Some(ReadRequest::Versions),
            Action::Content { .. } => Some(ReadRequest::Content),
            _ => None,
        };
        if let Some(request) = self.active {
            *self.state(request) = RequestState::Loading;
        }
    }

    pub(super) fn finish(&mut self, success: bool) {
        if let Some(request) = self.active.take() {
            *self.state(request) = if success {
                RequestState::Ready
            } else {
                RequestState::Failed
            };
        }
    }

    fn state(&mut self, request: ReadRequest) -> &mut RequestState {
        match request {
            ReadRequest::Versions => &mut self.versions,
            ReadRequest::Content => &mut self.content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::ContentKind;

    #[test]
    fn failure_stays_visible_until_explicit_retry_and_does_not_poison_other_requests() {
        let mut requests = ReadRequests::default();
        requests.begin(&Action::Versions);
        requests.finish(false);
        assert_eq!(requests.versions, RequestState::Failed);
        requests.begin(&Action::Content {
            id: "demo".into(),
            kind: ContentKind::Mods,
        });
        requests.finish(true);
        assert_eq!(requests.versions, RequestState::Failed);
        assert_eq!(requests.content, RequestState::Ready);
        requests.begin(&Action::Versions);
        assert_eq!(requests.versions, RequestState::Loading);
        requests.finish(true);
        assert_eq!(requests.versions, RequestState::Ready);
        requests.finish(false); // A later unrelated completion cannot overwrite it.
        assert_eq!(requests.versions, RequestState::Ready);
    }
}
