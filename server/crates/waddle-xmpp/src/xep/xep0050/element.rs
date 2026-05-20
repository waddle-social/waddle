use minidom::Element;

use crate::xep::xep0004::{DataForm, FromElement, IntoElement, NS_DATA_FORMS};

use super::{Action, AllowedActions, Command, CommandError, Note, NoteType, Status, NS_COMMANDS};

// ---------------------------------------------------------------------------
// FromElement / IntoElement
// ---------------------------------------------------------------------------

impl FromElement for Note {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let note_type = elem
            .attr("type")
            .map(|t| t.parse::<NoteType>())
            .transpose()?
            .unwrap_or_default();

        Ok(Self {
            note_type,
            text: elem.text(),
        })
    }
}

impl IntoElement for Note {
    fn into_element(&self) -> Element {
        let mut builder = Element::builder("note", NS_COMMANDS).attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            self.note_type.as_str(),
        );
        builder = builder.append(minidom::Node::Text(self.text.clone()));
        builder.build()
    }
}

impl FromElement for AllowedActions {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let execute_default = elem
            .attr("execute")
            .map(|a| a.parse::<Action>())
            .transpose()?
            .unwrap_or(Action::Execute);

        let prev = elem.children().any(|c| c.name() == "prev");
        let next = elem.children().any(|c| c.name() == "next");
        let complete = elem.children().any(|c| c.name() == "complete");

        Ok(Self {
            execute_default,
            prev,
            next,
            complete,
        })
    }
}

impl IntoElement for AllowedActions {
    fn into_element(&self) -> Element {
        let mut builder = Element::builder("actions", NS_COMMANDS).attr(
            minidom::rxml::xml_ncname!("execute").to_owned(),
            self.execute_default.as_str(),
        );

        if self.prev {
            builder = builder.append(Element::builder("prev", NS_COMMANDS).build());
        }
        if self.next {
            builder = builder.append(Element::builder("next", NS_COMMANDS).build());
        }
        if self.complete {
            builder = builder.append(Element::builder("complete", NS_COMMANDS).build());
        }

        builder.build()
    }
}

impl FromElement for Command {
    type Error = CommandError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != "command" || elem.ns() != NS_COMMANDS {
            return Err(CommandError::NotACommand);
        }

        let node = elem
            .attr("node")
            .ok_or(CommandError::MissingNode)?
            .to_string();

        let session_id = elem.attr("sessionid").map(|s| s.to_string());

        let action = elem
            .attr("action")
            .map(|a| a.parse::<Action>())
            .transpose()?;

        let status = elem
            .attr("status")
            .map(|s| s.parse::<Status>())
            .transpose()?;

        let actions = elem
            .children()
            .find(|c| c.name() == "actions" && c.ns() == NS_COMMANDS)
            .map(AllowedActions::from_element)
            .transpose()?;

        let notes = elem
            .children()
            .filter(|c| c.name() == "note" && c.ns() == NS_COMMANDS)
            .map(Note::from_element)
            .collect::<Result<Vec<_>, _>>()?;

        let form = elem
            .children()
            .find(|c| c.name() == "x" && c.ns() == NS_DATA_FORMS)
            .map(DataForm::from_element)
            .transpose()?;

        Ok(Self {
            node,
            session_id,
            action,
            status,
            actions,
            notes,
            form,
        })
    }
}

impl IntoElement for Command {
    fn into_element(&self) -> Element {
        let mut builder = Element::builder("command", NS_COMMANDS)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), &self.node);

        if let Some(ref sid) = self.session_id {
            builder = builder.attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), sid);
        }

        if let Some(action) = self.action {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("action").to_owned(),
                action.as_str(),
            );
        }

        if let Some(status) = self.status {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("status").to_owned(),
                status.as_str(),
            );
        }

        if let Some(ref actions) = self.actions {
            builder = builder.append(actions.into_element());
        }

        for note in &self.notes {
            builder = builder.append(note.into_element());
        }

        if let Some(ref form) = self.form {
            builder = builder.append(form.into_element());
        }

        builder.build()
    }
}
