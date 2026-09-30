//! The typed answer set the decode tests and the decode allocation budget
//! share.

use super::*;

/// A question set declared as a struct, with the [`AnswerSet`] implementation
/// the derive generates, written by hand: field dispatch on the key, no map
/// and no name string.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Ticket {
    pub(super) spam: NoulAnswer,
    pub(super) tone: ChoiceAnswer,
    pub(super) quality: ScoreAnswer,
}

enum TicketField {
    Spam,
    Tone,
    Quality,
    Other,
}

impl<'de> Deserialize<'de> for TicketField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;

        impl Visitor<'_> for FieldVisitor {
            type Value = TicketField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a question name")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<TicketField, E> {
                Ok(match value {
                    "spam" => TicketField::Spam,
                    "tone" => TicketField::Tone,
                    "quality" => TicketField::Quality,
                    _ => TicketField::Other,
                })
            }
        }

        deserializer.deserialize_str(FieldVisitor)
    }
}

impl AnswerSet for Ticket {
    fn deserialize_answers<'de, D>(deserializer: D, _: AnswerContext) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TicketVisitor;

        impl<'de> Visitor<'de> for TicketVisitor {
            type Value = Ticket;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the answers of a Ticket")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Ticket, M::Error>
            where
                M: MapAccess<'de>,
            {
                let (mut spam, mut tone, mut quality) = (None, None, None);
                while let Some(field) = map.next_key::<TicketField>()? {
                    match field {
                        TicketField::Spam if spam.is_none() => spam = Some(map.next_value()?),
                        TicketField::Tone if tone.is_none() => tone = Some(map.next_value()?),
                        TicketField::Quality if quality.is_none() => {
                            quality = Some(map.next_value()?);
                        }
                        // An answer the struct has no field for, or a later
                        // answer to a question already read: the first answer
                        // of a name is the one kept.
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(Ticket {
                    spam: spam.ok_or_else(|| de::Error::missing_field("spam"))?,
                    tone: tone.ok_or_else(|| de::Error::missing_field("tone"))?,
                    quality: quality.ok_or_else(|| de::Error::missing_field("quality"))?,
                })
            }
        }

        deserializer.deserialize_map(TicketVisitor)
    }
}
