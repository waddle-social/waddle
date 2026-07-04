use minidom::Element;

use super::*;

/// Trait for types that can be parsed from a minidom `Element`.
pub trait FromElement: Sized {
    /// The error type returned on parse failure.
    type Error;

    /// Parse from a minidom `Element`.
    fn from_element(elem: &Element) -> Result<Self, Self::Error>;
}

/// Trait for types that can be serialized to a minidom `Element`.
pub trait ToElement {
    /// Serialize into a minidom `Element`.
    fn to_element(&self) -> Element;
}

impl FromElement for FieldOption {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let label = elem.attr("label").map(|s| s.to_string());
        let value = elem
            .get_child("value", NS_DATA_FORMS)
            .map(|v| v.text())
            .unwrap_or_default();

        Ok(Self { label, value })
    }
}

impl ToElement for FieldOption {
    fn to_element(&self) -> Element {
        let mut builder = Element::builder("option", NS_DATA_FORMS);

        if let Some(ref label) = self.label {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                label.as_str(),
            );
        }

        builder
            .append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(self.value.as_str())
                    .build(),
            )
            .build()
    }
}

impl FromElement for Field {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        let var = elem.attr("var").map(|s| s.to_string());

        let field_type = match elem.attr("type") {
            Some(t) => t.parse()?,
            None => FieldType::TextSingle,
        };

        let label = elem.attr("label").map(|s| s.to_string());

        let desc = elem
            .get_child("desc", NS_DATA_FORMS)
            .map(|d| d.text())
            .filter(|t| !t.is_empty());

        let required = elem.get_child("required", NS_DATA_FORMS).is_some();

        let values: Vec<String> = elem
            .children()
            .filter(|c| c.name() == "value" && c.ns() == NS_DATA_FORMS)
            .map(|v| v.text())
            .collect();

        let options: Vec<FieldOption> = elem
            .children()
            .filter(|c| c.name() == "option" && c.ns() == NS_DATA_FORMS)
            .filter_map(|o| FieldOption::from_element(o).ok())
            .collect();

        Ok(Self {
            var,
            field_type,
            label,
            desc,
            required,
            values,
            options,
        })
    }
}

impl ToElement for Field {
    fn to_element(&self) -> Element {
        let mut builder = Element::builder("field", NS_DATA_FORMS).attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            self.field_type.as_str(),
        );

        if let Some(ref var) = self.var {
            builder = builder.attr(minidom::rxml::xml_ncname!("var").to_owned(), var.as_str());
        }

        if let Some(ref label) = self.label {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("label").to_owned(),
                label.as_str(),
            );
        }

        if let Some(ref desc) = self.desc {
            builder = builder.append(
                Element::builder("desc", NS_DATA_FORMS)
                    .append(desc.as_str())
                    .build(),
            );
        }

        if self.required {
            builder = builder.append(Element::builder("required", NS_DATA_FORMS).build());
        }

        for value in &self.values {
            builder = builder.append(
                Element::builder("value", NS_DATA_FORMS)
                    .append(value.as_str())
                    .build(),
            );
        }

        for option in &self.options {
            builder = builder.append(option.to_element());
        }

        builder.build()
    }
}

impl FromElement for DataForm {
    type Error = DataFormError;

    fn from_element(elem: &Element) -> Result<Self, Self::Error> {
        if elem.name() != "x" || elem.ns() != NS_DATA_FORMS {
            return Err(DataFormError::NotADataForm);
        }

        let form_type: FormType = elem
            .attr("type")
            .ok_or_else(|| DataFormError::MissingElement("type attribute".to_string()))?
            .parse()?;

        let title = elem
            .get_child("title", NS_DATA_FORMS)
            .map(|t| t.text())
            .filter(|t| !t.is_empty());

        let instructions: Vec<String> = elem
            .children()
            .filter(|c| c.name() == "instructions" && c.ns() == NS_DATA_FORMS)
            .map(|i| i.text())
            .collect();

        let fields: Vec<Field> = elem
            .children()
            .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
            .filter_map(|f| Field::from_element(f).ok())
            .collect();

        let reported: Vec<Field> = elem
            .get_child("reported", NS_DATA_FORMS)
            .map(|r| {
                r.children()
                    .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
                    .filter_map(|f| Field::from_element(f).ok())
                    .collect()
            })
            .unwrap_or_default();

        let items: Vec<Vec<Field>> = elem
            .children()
            .filter(|c| c.name() == "item" && c.ns() == NS_DATA_FORMS)
            .map(|item| {
                item.children()
                    .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
                    .filter_map(|f| Field::from_element(f).ok())
                    .collect()
            })
            .collect();

        Ok(Self {
            form_type,
            title,
            instructions,
            fields,
            reported,
            items,
        })
    }
}

impl ToElement for DataForm {
    fn to_element(&self) -> Element {
        let mut builder = Element::builder("x", NS_DATA_FORMS).attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            self.form_type.as_str(),
        );

        if let Some(ref title) = self.title {
            builder = builder.append(
                Element::builder("title", NS_DATA_FORMS)
                    .append(title.as_str())
                    .build(),
            );
        }

        for instruction in &self.instructions {
            builder = builder.append(
                Element::builder("instructions", NS_DATA_FORMS)
                    .append(instruction.as_str())
                    .build(),
            );
        }

        for field in &self.fields {
            builder = builder.append(field.to_element());
        }

        if !self.reported.is_empty() {
            let mut reported_builder = Element::builder("reported", NS_DATA_FORMS);
            for field in &self.reported {
                reported_builder = reported_builder.append(field.to_element());
            }
            builder = builder.append(reported_builder.build());
        }

        for item_fields in &self.items {
            let mut item_builder = Element::builder("item", NS_DATA_FORMS);
            for field in item_fields {
                item_builder = item_builder.append(field.to_element());
            }
            builder = builder.append(item_builder.build());
        }

        builder.build()
    }
}

// ---------------------------------------------------------------------------
// Convenience: Check if an Element is a data form
// ---------------------------------------------------------------------------

/// Check whether a minidom `Element` is a `<x xmlns='jabber:x:data'>` form.
pub fn is_data_form(elem: &Element) -> bool {
    elem.name() == "x" && elem.ns() == NS_DATA_FORMS
}

/// Try to extract a `DataForm` from a parent element's children.
pub fn find_data_form(parent: &Element) -> Option<Result<DataForm, DataFormError>> {
    parent
        .children()
        .find(|c| is_data_form(c))
        .map(DataForm::from_element)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
