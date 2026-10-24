//! A step's answer: the text of its last assistant message that has any. Earlier messages narrate
//! tool use, or are answers a completion gate sent back for more work; they are not passed on.
use harness_protocol::ids::MessageId;

#[derive(Default)]
pub(super) struct AnswerText {
    current: String,
    current_id: Option<MessageId>,
    last: Option<String>,
}

impl AnswerText {
    /// A text delta of message `id`. A delta of another message closes the current one.
    pub(super) fn delta(&mut self, id: MessageId, delta: &str) {
        if self.current_id != Some(id) {
            self.close();
            self.current_id = Some(id);
        }
        self.current.push_str(delta);
    }

    /// The current message ended (or a new one started without a delta yet).
    pub(super) fn close(&mut self) {
        if self.current.trim().is_empty() {
            self.current.clear();
        } else {
            self.last = Some(std::mem::take(&mut self.current));
        }
        self.current_id = None;
    }

    pub(super) fn finish(mut self) -> String {
        self.close();
        self.last.unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_message_with_text_is_the_answer() {
        let (narration, first, second) = (MessageId::new(), MessageId::new(), MessageId::new());
        let mut answer = AnswerText::default();
        answer.delta(narration, "Let me read the files.");
        answer.delta(first, "# Spec\n");
        answer.delta(first, "R1");
        // A completion gate sent it back; the step answers again in full.
        answer.delta(second, "# Spec\nR1 and R2");
        assert_eq!(answer.finish(), "# Spec\nR1 and R2");
    }

    #[test]
    fn a_message_without_text_does_not_replace_the_answer() {
        let (answer_id, empty) = (MessageId::new(), MessageId::new());
        let mut answer = AnswerText::default();
        answer.delta(answer_id, "# Spec");
        answer.close();
        answer.delta(empty, "  \n");
        assert_eq!(answer.finish(), "# Spec");
        assert_eq!(AnswerText::default().finish(), "");
    }
}
