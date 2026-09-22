#[derive(Clone, Debug, Default)]
pub(crate) struct ActivationReviewNotifier;

impl ActivationReviewNotifier {
    pub(crate) async fn notify(&self, _review_id: &str) {}
}
