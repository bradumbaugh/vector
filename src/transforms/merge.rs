use super::Transform;
use crate::runtime::TaskExecutor;
use crate::{
    event::merge::merge_log_event,
    event::{self, Event},
    topology::config::{DataType, TransformConfig, TransformDescription},
};
use serde::{Deserialize, Serialize};
use string_cache::DefaultAtom as Atom;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct MergeConfig {
    /// The field that indicates that the event is partial. A consequent stream
    /// of partial events along with the first non-partial event will be merged
    /// together.
    pub partial_event_marker: Atom,
    /// Fields to merge. The values of these fields will be merged into the
    /// first partial event. Fields not specified here will be ignored.
    /// Merging process takes the first buffered partial event, then loops over
    /// the rest of them and merges in the fields from each buffered partial
    /// event.
    /// Finally, the non-partial event fields are merged in, producing the
    /// resulting merged event.
    pub merge_fields: Vec<Atom>,
}

inventory::submit! {
    TransformDescription::new::<MergeConfig>("merge")
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            partial_event_marker: event::PARTIAL.clone(),
            merge_fields: vec![event::MESSAGE.clone()],
        }
    }
}

#[typetag::serde(name = "merge")]
impl TransformConfig for MergeConfig {
    fn build(&self, _exec: TaskExecutor) -> crate::Result<Box<dyn Transform>> {
        Ok(Box::new(Merge::from(self.clone())))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn output_type(&self) -> DataType {
        DataType::Log
    }

    fn transform_type(&self) -> &'static str {
        "merge"
    }
}

#[derive(Debug)]
pub struct Merge {
    partial_event_marker: Atom,
    merge_fields: Vec<Atom>,
    partial_events: Vec<Event>,
}

impl From<MergeConfig> for Merge {
    fn from(config: MergeConfig) -> Self {
        Self {
            partial_event_marker: config.partial_event_marker,
            merge_fields: config.merge_fields,
            partial_events: Vec::new(),
        }
    }
}

impl Transform for Merge {
    fn transform(&mut self, mut event: Event) -> Option<Event> {
        // TODO: `lua` transform doesn't support assigning non-string values.
        // Normally we'd check for the field value to be `true`, and only then
        // consider event partial, but, to simplify the integration, for now we
        // only check for the field presence. We can switch this to check the
        // value to be `true` when the `lua` supports setting boolean fields
        // easily, as we expect users to rely on `lua` transform to implement
        // custom partial markers.

        // If current event has the partial marker, consider it partial.
        // Remove the partial marker from the event and stash it.
        if event
            .as_mut_log()
            .remove(&self.partial_event_marker)
            .is_some()
        {
            self.partial_events.push(event);
            return None;
        }

        // Short circut to returning the event as is if there're no pending
        // events.
        if self.partial_events.is_empty() {
            return Some(event);
        }

        let merge_fields = self.merge_fields.as_slice();

        // Merge all partial events.
        let mut drain = self.partial_events.drain(..);

        // Take the first partial event. We know this won't fail cause we
        // checked that partial events list is not empty earlier.
        let mut merged_event = drain.next().unwrap();

        // Merge all partial events into the merge event.
        for partial_event in drain {
            merge_log_event(
                &mut merged_event.as_mut_log(),
                partial_event.into_log(),
                merge_fields,
            );
        }

        // Merge the current event last.
        merge_log_event(
            &mut merged_event.as_mut_log(),
            event.into_log(),
            merge_fields,
        );

        // Return the merged event.
        Some(merged_event)
    }
}

#[cfg(test)]
mod test {
    use super::{Merge, MergeConfig};
    use crate::event::{self, Event};
    use crate::transforms::Transform;
    use string_cache::DefaultAtom as Atom;

    fn make_partial(mut event: Event) -> Event {
        event
            .as_mut_log()
            .insert_explicit(event::PARTIAL.clone(), true);
        event
    }

    #[test]
    fn merge_passthorughs_non_partial_events() {
        let mut merge = Merge::from(MergeConfig::default());

        // A non-partial event.
        let sample_event = Event::from("hello world");

        // Once processed by the transform.
        let merged_event = merge.transform(sample_event.clone()).unwrap();

        // Should be returned as is.
        assert_eq!(merged_event, sample_event);
    }

    #[test]
    fn merge_merges_partial_events() {
        let mut merge = Merge::from(MergeConfig::default());

        let partial_event_1 = make_partial(Event::from("hel"));
        let partial_event_2 = make_partial(Event::from("lo "));
        let non_partial_event = Event::from("world");

        assert!(merge.transform(partial_event_1).is_none());
        assert!(merge.transform(partial_event_2).is_none());
        let merged_event = merge.transform(non_partial_event).unwrap();

        assert_eq!(
            merged_event
                .as_log()
                .get(&Atom::from("message"))
                .unwrap()
                .as_bytes()
                .as_ref(),
            b"hello world"
        );

        // Merged event shouldn't contain partial event marker.
        assert!(!merged_event.as_log().contains(&event::PARTIAL));
    }
}
