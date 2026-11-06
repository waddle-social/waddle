// Copyright (c) 2022 Astro <astro@spaceboyz.net>
// Copyright (c) 2025 pep <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! SAX events to DOM tree conversion

use crate::prefixes::{Prefix, Prefixes};
use crate::{Element, Error};
use alloc::string::String;
use alloc::vec::Vec;
use rxml::{AttrMap, Namespace, RawEvent};

/// Tree-building parser state
pub struct TreeBuilder {
    next_tag: Option<(Prefix, String, Prefixes, AttrMap)>,
    /// Parsing stack
    stack: Vec<Element>,
    /// Namespace set stack by prefix
    prefixes_stack: Vec<Prefixes>,
    attrs_stack: Vec<(String, String, String)>,
    /// Document root element if finished
    pub root: Option<Element>,
}

impl Default for TreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TreeBuilder {
    /// Create a new one
    #[must_use]
    pub fn new() -> Self {
        TreeBuilder {
            next_tag: None,
            stack: Vec::new(),
            prefixes_stack: Vec::new(),
            attrs_stack: Vec::new(),
            root: None,
        }
    }

    /// Allow setting prefixes stack.
    ///
    /// Useful to provide knowledge of namespaces that would have been declared on parent elements
    /// not present in the reader.
    #[must_use]
    pub fn with_prefixes_stack(mut self, prefixes_stack: Vec<Prefixes>) -> Self {
        self.prefixes_stack = prefixes_stack;
        self
    }

    /// Stack depth
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Get the top-most element from the stack but don't remove it
    #[must_use]
    pub fn top(&mut self) -> Option<&Element> {
        self.stack.last()
    }

    /// Pop the top-most element from the stack
    fn pop(&mut self) -> Option<Element> {
        self.prefixes_stack.pop();
        self.stack.pop()
    }

    /// Unshift the first child of the top element
    pub fn unshift_child(&mut self) -> Option<Element> {
        let depth = self.stack.len();
        if depth > 0 {
            self.stack[depth - 1].unshift_child()
        } else {
            None
        }
    }

    /// Lookup XML namespace declaration for given prefix (or no prefix)
    #[must_use]
    fn lookup_prefix<'a>(
        prefixes_stack: &'a Vec<Prefixes>,
        prefix: &'a Option<String>,
    ) -> Option<&'a str> {
        for nss in prefixes_stack.iter().rev() {
            if let Some(ns) = nss.get(prefix) {
                return Some(ns);
            }
        }

        None
    }

    fn process_end_tag(&mut self) {
        if let Some(el) = self.pop() {
            if self.depth() > 0 {
                let top = self.stack.len() - 1;
                self.stack[top].append_child(el);
            } else {
                self.root = Some(el);
            }
        }
    }

    fn process_text(&mut self, text: String) {
        if self.depth() > 0 {
            let top = self.stack.len() - 1;
            self.stack[top].append_text(text);
        }
    }

    /// Process an event that you got out of a `RawParser`.
    pub fn process_event(&mut self, event: RawEvent) -> Result<(), Error> {
        match event {
            RawEvent::XmlDeclaration(_, _) => {}

            RawEvent::ElementHeadOpen(_, (prefix, name)) => {
                // If self.prefixes_stack has been set via with_prefixes_stack before processing,
                // ensure these are set on the root element.
                let prefixes = if self.stack.is_empty() && self.prefixes_stack.len() == 1 {
                    self.prefixes_stack.pop().unwrap()
                } else {
                    Prefixes::default()
                };

                self.next_tag = Some((
                    prefix.map(|prefix| prefix.as_str().to_owned()),
                    name.as_str().to_owned(),
                    prefixes,
                    AttrMap::new(),
                ));
            }

            RawEvent::Attribute(_, (prefix, name), value) => {
                if let Some((_, _, ref mut prefixes, ref mut attrs)) = self.next_tag.as_mut() {
                    match (prefix, name) {
                        (None, xmlns) if xmlns == "xmlns" => prefixes.insert(None, value),
                        (Some(xmlns), prefix) if xmlns.as_str() == "xmlns" => {
                            prefixes.insert(Some(prefix.as_str().to_owned()), value);
                        }
                        (Some(prefix), name) => {
                            self.attrs_stack
                                .push((prefix.to_string(), name.to_string(), value));
                        }
                        (None, name) => {
                            attrs.insert(
                                Namespace::NONE,
                                name.try_into().unwrap(),
                                value.as_str().to_owned(),
                            );
                        }
                    }
                }
            }

            RawEvent::ElementHeadClose(_) => {
                if let Some((prefix, name, prefixes, mut attrs)) = self.next_tag.take() {
                    self.prefixes_stack.push(prefixes.clone());

                    let namespace = TreeBuilder::lookup_prefix(
                        &self.prefixes_stack,
                        &prefix.map(|prefix| prefix.as_str().to_owned()),
                    )
                    .ok_or(Error::MissingNamespace)?
                    .to_owned();

                    for (prefix, attr, value) in self.attrs_stack.drain(..) {
                        let ns = if prefix == "xml" {
                            rxml::Namespace::xml()
                        } else {
                            &TreeBuilder::lookup_prefix(
                                &self.prefixes_stack,
                                &Some(prefix.to_string()),
                            )
                            .map(String::from)
                            .map(|s| TryInto::<Namespace>::try_into(s).unwrap())
                            .ok_or(Error::MissingNamespace)?
                            .to_owned()
                        };
                        attrs.insert(ns.clone(), attr.try_into().unwrap(), value.to_string());
                    }

                    let el = Element::new(name.to_owned(), namespace, prefixes, attrs, Vec::new());
                    self.stack.push(el);
                }
            }

            RawEvent::ElementFoot(_) => self.process_end_tag(),

            RawEvent::Text(_, text) => self.process_text(text.as_str().to_owned()),
        }

        Ok(())
    }
}
