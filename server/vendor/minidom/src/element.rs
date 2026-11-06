// Copyright (c) 2020 lumi <lumi@pew.im>
// Copyright (c) 2020 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2020 Bastien Orivel <eijebong+minidom@bananium.fr>
// Copyright (c) 2020 Maxime “pep” Buquet <pep@bouah.net>
// Copyright (c) 2020 Yue Liu <amznyue@amazon.com>
// Copyright (c) 2020 Matt Bilker <me@mbilker.us>
// Copyright (c) 2020 Xidorn Quan <me@upsuper.org>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Provides an `Element` type, which represents DOM nodes, and a builder to create them with.

use crate::convert::IntoAttributeValue;
use crate::error::{Error, Result};
use crate::namespaces::NSChoice;
use crate::node::Node;
use crate::prefixes::{Namespace, Prefix, Prefixes};
use crate::tree_builder::TreeBuilder;

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use core::slice;
use core::str::FromStr;

use std::borrow::Borrow;
use std::hash::Hash;
use std::io;

use rxml::writer::{Encoder, Item, TrackNamespace};
use rxml::{AttrMap, Namespace as RxmlNamespace, NcName, RawReader, XmlVersion};

fn encode_and_write<W: io::Write, T: rxml::writer::TrackNamespace>(
    item: Item<'_>,
    enc: &mut Encoder<T>,
    mut w: W,
) -> io::Result<()> {
    let mut buf = rxml::bytes::BytesMut::new();
    enc.encode_into_bytes(item, &mut buf)
        .expect("encoder driven incorrectly");
    w.write_all(&buf[..])?;
    Ok(())
}

/// Wrapper around a [`io::Write`] and an [`rxml::writer::Encoder`], to
/// provide a simple function to write an rxml Item to a writer.
pub struct CustomItemWriter<W, T> {
    writer: W,
    encoder: Encoder<T>,
}

impl<W: io::Write> CustomItemWriter<W, rxml::writer::SimpleNamespaces> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            encoder: Encoder::new(),
        }
    }
}

impl<W: io::Write, T: rxml::writer::TrackNamespace> CustomItemWriter<W, T> {
    pub(crate) fn write(&mut self, item: Item<'_>) -> io::Result<()> {
        encode_and_write(item, &mut self.encoder, &mut self.writer)
    }
}

/// Type alias to simplify the use for the default namespace tracking
/// implementation.
pub type ItemWriter<W> = CustomItemWriter<W, rxml::writer::SimpleNamespaces>;

/// helper function to escape a `&[u8]` and replace all
/// xml special characters (<, >, &, ', ") with their corresponding
/// xml escaped value.
#[must_use]
pub fn escape(raw: &[u8]) -> Cow<'_, [u8]> {
    fn to_escape(b: u8) -> bool {
        matches!(b, b'<' | b'>' | b'\'' | b'&' | b'"')
    }

    let mut escapes: Vec<(usize, &'static [u8])> = Vec::new();
    let mut bytes = raw.iter();
    let mut loc = 0;
    while let Some(i) = bytes.position(|&b| to_escape(b)) {
        loc += i;
        match raw[loc] {
            b'<' => escapes.push((loc, b"&lt;")),
            b'>' => escapes.push((loc, b"&gt;")),
            b'\'' => escapes.push((loc, b"&apos;")),
            b'&' => escapes.push((loc, b"&amp;")),
            b'"' => escapes.push((loc, b"&quot;")),
            _ => unreachable!("Only '<', '>', '\'', '&' and '\"' are escaped"),
        }
        loc += 1;
    }

    if escapes.is_empty() {
        Cow::Borrowed(raw)
    } else {
        let len = raw.len();
        let mut v = Vec::with_capacity(len);
        let mut start = 0;
        for (i, r) in escapes {
            v.extend_from_slice(&raw[start..i]);
            v.extend_from_slice(r);
            start = i + 1;
        }

        if start < len {
            v.extend_from_slice(&raw[start..]);
        }
        Cow::Owned(v)
    }
}

#[derive(Eq, Debug)]
/// A struct representing a DOM Element.
pub struct Element {
    name: String,
    namespace: String,
    /// Namespace declarations
    pub prefixes: Prefixes,
    attributes: AttrMap,
    children: Vec<Node>,
}

impl Clone for Element {
    fn clone(&self) -> Self {
        struct Frame<'a> {
            source: &'a Element,
            cloned: Element,
            next_child: usize,
        }

        fn clone_header(source: &Element) -> Element {
            Element {
                name: source.name.clone(),
                namespace: source.namespace.clone(),
                prefixes: source.prefixes.clone(),
                attributes: source.attributes.clone(),
                children: Vec::new(),
            }
        }

        let mut stack = vec![Frame {
            source: self,
            cloned: clone_header(self),
            next_child: 0,
        }];

        loop {
            let next_child = {
                let Some(frame) = stack.last_mut() else {
                    unreachable!("the root clone frame returns the completed element")
                };
                if frame.next_child == frame.source.children.len() {
                    None
                } else {
                    let source = frame.source;
                    let child = &source.children[frame.next_child];
                    frame.next_child += 1;
                    Some(child)
                }
            };

            match next_child {
                Some(Node::Text(text)) => {
                    let Some(frame) = stack.last_mut() else {
                        unreachable!("a child can only be read from an active frame")
                    };
                    frame.cloned.children.push(Node::Text(text.clone()));
                }
                Some(Node::Element(child)) => stack.push(Frame {
                    source: child,
                    cloned: clone_header(child),
                    next_child: 0,
                }),
                None => {
                    let Some(frame) = stack.pop() else {
                        unreachable!("the root clone frame returns the completed element")
                    };
                    if let Some(parent) = stack.last_mut() {
                        parent.cloned.children.push(Node::Element(frame.cloned));
                    } else {
                        return frame.cloned;
                    }
                }
            }
        }
    }
}

impl Drop for Element {
    fn drop(&mut self) {
        let mut pending = vec![core::mem::take(&mut self.children).into_iter()];

        while let Some(frame) = pending.last_mut() {
            match frame.next() {
                Some(Node::Element(mut child)) => {
                    pending.push(core::mem::take(&mut child.children).into_iter());
                    drop(child);
                }
                Some(Node::Text(text)) => drop(text),
                None => {
                    pending.pop();
                }
            }
        }
    }
}

impl<'a> From<&'a Element> for String {
    fn from(elem: &'a Element) -> String {
        let mut writer = Vec::new();
        elem.write_to(&mut writer).unwrap();
        String::from_utf8(writer).unwrap()
    }
}

impl FromStr for Element {
    type Err = Error;

    fn from_str(s: &str) -> Result<Element> {
        Element::from_reader(s.as_bytes())
    }
}

impl PartialEq for Element {
    fn eq(&self, other: &Self) -> bool {
        if self.name() == other.name() && self.ns() == other.ns() && self.attrs().eq(other.attrs())
        {
            if self.nodes().count() != other.nodes().count() {
                return false;
            }
            self.nodes()
                .zip(other.nodes())
                .all(|(node1, node2)| node1 == node2)
        } else {
            false
        }
    }
}

impl Element {
    pub(crate) fn new<P: Into<Prefixes>>(
        name: String,
        namespace: String,
        prefixes: P,
        attributes: AttrMap,
        children: Vec<Node>,
    ) -> Element {
        Element {
            name,
            namespace,
            prefixes: prefixes.into(),
            attributes,
            children,
        }
    }

    /// Return a builder for an `Element` with the given `name`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    /// use rxml::{Namespace, xml_ncname};
    ///
    /// let elem = Element::builder("name", "namespace")
    ///                    .attr(xml_ncname!("name").to_owned(), "value")
    ///                    .append("inner")
    ///                    .build();
    ///
    /// assert_eq!(elem.name(), "name");
    /// assert_eq!(elem.ns(), "namespace".to_owned());
    /// assert_eq!(elem.attr("name"), Some("value"));
    /// assert_eq!(elem.attr("inexistent"), None);
    /// assert_eq!(elem.text(), "inner");
    /// ```
    pub fn builder<S: AsRef<str>, NS: Into<String>>(name: S, namespace: NS) -> ElementBuilder {
        ElementBuilder {
            root: Element::new(
                name.as_ref().to_string(),
                namespace.into(),
                None,
                AttrMap::new(),
                Vec::new(),
            ),
        }
    }

    /// Returns a bare minimum `Element` with this name.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    /// use rxml::Namespace;
    ///
    /// let bare = Element::bare("name", "namespace");
    ///
    /// assert_eq!(bare.name(), "name");
    /// assert_eq!(bare.ns(), "namespace");
    /// assert_eq!(bare.attr("name"), None);
    /// assert_eq!(bare.text(), "");
    /// ```
    pub fn bare<S: Into<String>, NS: Into<String>>(name: S, namespace: NS) -> Element {
        Element::new(
            name.into(),
            namespace.into(),
            None,
            AttrMap::new(),
            Vec::new(),
        )
    }

    /// Returns a reference to the local name of this element (that is, without a possible prefix).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns a reference to the namespace of this element.
    #[must_use]
    pub fn ns(&self) -> String {
        self.namespace.clone()
    }

    /// Return a borrowed reference to the namespace of this element.
    ///
    /// This is useful when inspecting untrusted XML without cloning an
    /// attacker-controlled namespace string.
    #[must_use]
    pub fn ns_ref(&self) -> &str {
        &self.namespace
    }

    /// Returns a reference to the value of the given attribute, if it exists, else `None`.
    #[must_use]
    pub fn attr<'a, N: Ord + Hash + Eq + ?Sized>(&'a self, name: &'a N) -> Option<&'a str>
    where
        NcName: Borrow<N>,
    {
        if let Some(value) = self.attributes.get(&RxmlNamespace::NONE, name) {
            return Some(value);
        }
        None
    }

    /// Returns a reference to the value of the given namespaced attribute, if it exists, else `None`.
    #[must_use]
    pub fn attr_ns<'a, NS: Ord + Hash + Eq + ?Sized, N: Ord + Hash + Eq + ?Sized>(
        &'a self,
        ns: &'a NS,
        name: &'a N,
    ) -> Option<&'a str>
    where
        RxmlNamespace: Borrow<NS>,
        NcName: Borrow<N>,
    {
        if let Some(value) = self.attributes.get(ns, name) {
            return Some(value);
        }
        None
    }

    /// Returns an iterator over the attributes of this element.
    ///
    /// # Example
    ///
    /// ```rust
    /// use minidom::Element;
    /// use rxml::{Namespace, xml_ncname};
    ///
    /// let elm: Element = "<elem xmlns=\"ns1\" a=\"b\" />".parse().unwrap();
    ///
    /// let mut iter = elm.attrs().iter();
    ///
    /// assert_eq!(iter.next().unwrap(), ((&Namespace::NONE, &xml_ncname!("a").to_owned()), &String::from("b")));
    /// assert_eq!(iter.next(), None);
    /// ```
    #[must_use]
    pub fn attrs(&self) -> &AttrMap {
        &self.attributes
    }

    /// Returns an iterator over the attributes of this element, with the value being a mutable
    /// reference.
    #[must_use]
    pub fn attrs_mut(&mut self) -> &mut AttrMap {
        &mut self.attributes
    }

    /// Modifies the value of an attribute.
    pub fn set_attr<V: IntoAttributeValue>(&mut self, ns: RxmlNamespace, name: NcName, val: V) {
        let val = val.into_attribute_value();

        if let Some(value) = self.attributes.get_mut(&ns, &name) {
            *value = val
                .expect("removing existing value via set_attr, this is not yet supported (TODO)"); // TODO
            return;
        }

        if let Some(val) = val {
            self.attributes.insert(ns.clone(), name, val);
        }
    }

    /// Returns whether the element has the given name and namespace.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, NSChoice};
    ///
    /// let elem = Element::builder("name", "namespace").build();
    ///
    /// assert_eq!(elem.is("name", "namespace"), true);
    /// assert_eq!(elem.is("name", "wrong"), false);
    /// assert_eq!(elem.is("wrong", "namespace"), false);
    /// assert_eq!(elem.is("wrong", "wrong"), false);
    ///
    /// assert_eq!(elem.is("name", NSChoice::OneOf("namespace")), true);
    /// assert_eq!(elem.is("name", NSChoice::OneOf("foo")), false);
    /// assert_eq!(elem.is("name", NSChoice::AnyOf(&["foo", "namespace"])), true);
    /// assert_eq!(elem.is("name", NSChoice::Any), true);
    /// ```
    pub fn is<'a, N: AsRef<str>, NS: Into<NSChoice<'a>>>(&self, name: N, namespace: NS) -> bool {
        self.name == name.as_ref() && namespace.into().compare(self.namespace.as_ref())
    }

    /// Returns whether the element has the given namespace.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, NSChoice};
    ///
    /// let elem = Element::builder("name", "namespace").build();
    ///
    /// assert_eq!(elem.has_ns("namespace"), true);
    /// assert_eq!(elem.has_ns("wrong"), false);
    ///
    /// assert_eq!(elem.has_ns(NSChoice::OneOf("namespace")), true);
    /// assert_eq!(elem.has_ns(NSChoice::OneOf("foo")), false);
    /// assert_eq!(elem.has_ns(NSChoice::AnyOf(&["foo", "namespace"])), true);
    /// assert_eq!(elem.has_ns(NSChoice::Any), true);
    /// ```
    pub fn has_ns<'a, NS: Into<NSChoice<'a>>>(&self, namespace: NS) -> bool {
        namespace.into().compare(self.namespace.as_ref())
    }

    fn from_reader_inner<R: io::BufRead>(
        mut tree_builder: TreeBuilder,
        reader: R,
    ) -> Result<Element> {
        let mut driver = RawReader::new(reader);
        while let Some(event) = driver.read()? {
            tree_builder.process_event(event)?;

            if let Some(root) = tree_builder.root.take() {
                return Ok(root);
            }
        }
        Err(Error::EndOfDocument)
    }

    /// Parse a document from a `io::BufRead`.
    pub fn from_reader<R: io::BufRead>(reader: R) -> Result<Element> {
        Element::from_reader_inner(TreeBuilder::new(), reader)
    }

    /// Parse a document from a `io::BufRead`, allowing Prefixes to be specified. Useful to provide
    /// knowledge of namespaces that would have been declared on parent elements not present in the
    /// reader.
    pub fn from_reader_with_prefixes<R: io::BufRead, P: Into<Prefixes>>(
        reader: R,
        prefixes: P,
    ) -> Result<Element> {
        let tree_builder = TreeBuilder::new().with_prefixes_stack([prefixes.into()].into());
        Element::from_reader_inner(tree_builder, reader)
    }

    /// Output a document to a `io::Writer`.
    pub fn write_to<W: io::Write>(&self, writer: &mut W) -> Result<()> {
        self.to_writer(&mut ItemWriter::new(writer))
    }

    /// Output a document to a `io::Writer`.
    pub fn write_to_decl<W: io::Write>(&self, writer: &mut W) -> Result<()> {
        self.to_writer_decl(&mut ItemWriter::new(writer))
    }

    /// Output the document to an `ItemWriter`
    pub fn to_writer<W: io::Write>(&self, writer: &mut ItemWriter<W>) -> Result<()> {
        self.write_to_inner(writer)
    }

    /// Output the document to an `ItemWriter`
    pub fn to_writer_decl<W: io::Write>(&self, writer: &mut ItemWriter<W>) -> Result<()> {
        writer
            .write(Item::XmlDeclaration(XmlVersion::V1_0))
            .unwrap(); // TODO: error return
        self.write_to_inner(writer)
    }

    /// Like `write_to()` but without the `<?xml?>` prelude
    pub fn write_to_inner<W: io::Write>(&self, writer: &mut ItemWriter<W>) -> Result<()> {
        for (prefix, namespace) in self.prefixes.declared_prefixes() {
            assert!(writer.encoder.ns_tracker_mut().declare_fixed(
                prefix.as_deref().map(TryInto::try_into).transpose()?,
                namespace.clone().into(),
            ));
        }

        let namespace: RxmlNamespace = self.namespace.clone().into();
        writer.write(Item::ElementHeadStart(&namespace, (*self.name).try_into()?))?;

        for ((ns, key), value) in &self.attributes {
            writer.write(Item::Attribute(&ns, key, value))?;
        }

        if !self.children.is_empty() {
            writer.write(Item::ElementHeadEnd)?;
            for child in &self.children {
                child.write_to_inner(writer)?;
            }
        }
        writer.write(Item::ElementFoot)?;

        Ok(())
    }

    /// Extracts all children into a collection.
    pub fn take_nodes(&mut self) -> Vec<Node> {
        self.children.drain(..).collect()
    }

    /// Returns an iterator over references to every child node of this element.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let elem: Element = "<root xmlns=\"ns1\">a<c1 />b<c2 />c</root>".parse().unwrap();
    ///
    /// let mut iter = elem.nodes();
    ///
    /// assert_eq!(iter.next().unwrap().as_text().unwrap(), "a");
    /// assert_eq!(iter.next().unwrap().as_element().unwrap().name(), "c1");
    /// assert_eq!(iter.next().unwrap().as_text().unwrap(), "b");
    /// assert_eq!(iter.next().unwrap().as_element().unwrap().name(), "c2");
    /// assert_eq!(iter.next().unwrap().as_text().unwrap(), "c");
    /// assert_eq!(iter.next(), None);
    /// ```
    #[inline]
    pub fn nodes(&self) -> Nodes<'_> {
        self.children.iter()
    }

    /// Returns an iterator over mutable references to every child node of this element.
    #[inline]
    pub fn nodes_mut(&mut self) -> NodesMut<'_> {
        self.children.iter_mut()
    }

    /// Returns an iterator over references to every child element of this element.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let elem: Element = "<root xmlns=\"ns1\">hello<child1 xmlns=\"ns1\"/>this<child2 xmlns=\"ns1\"/>is<child3 xmlns=\"ns1\"/>ignored</root>".parse().unwrap();
    ///
    /// let mut iter = elem.children();
    /// assert_eq!(iter.next().unwrap().name(), "child1");
    /// assert_eq!(iter.next().unwrap().name(), "child2");
    /// assert_eq!(iter.next().unwrap().name(), "child3");
    /// assert_eq!(iter.next(), None);
    /// ```
    #[inline]
    #[must_use]
    pub fn children(&self) -> Children<'_> {
        Children {
            iter: self.children.iter(),
        }
    }

    /// Returns an iterator over mutable references to every child element of this element.
    #[inline]
    #[must_use]
    pub fn children_mut(&mut self) -> ChildrenMut<'_> {
        ChildrenMut {
            iter: self.children.iter_mut(),
        }
    }

    /// Returns an iterator over the child Elements, draining the element of
    /// all its child nodes **including text!**
    ///
    /// This is a bit of a footgun, so we make this hidden in the docs (it
    /// needs to be pub for macro use). Once `extract_if`
    /// ([rust#43244](https://github.com/rust-lang/rust/issues/43244))
    /// is stabilized, we can replace this with a take_children which doesn't
    /// remove text nodes.
    #[inline]
    #[doc(hidden)]
    pub fn take_contents_as_children(&mut self) -> ContentsAsChildren<'_> {
        ContentsAsChildren {
            iter: self.children.drain(..),
        }
    }

    /// Returns an iterator over references to every text node of this element.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let elem: Element = "<root xmlns=\"ns1\">hello<c /> world!</root>".parse().unwrap();
    ///
    /// let mut iter = elem.texts();
    /// assert_eq!(iter.next().unwrap(), "hello");
    /// assert_eq!(iter.next().unwrap(), " world!");
    /// assert_eq!(iter.next(), None);
    /// ```
    #[inline]
    #[must_use]
    pub fn texts(&self) -> Texts<'_> {
        Texts {
            iter: self.children.iter(),
        }
    }

    /// Returns an iterator over mutable references to every text node of this element.
    #[inline]
    #[must_use]
    pub fn texts_mut(&mut self) -> TextsMut<'_> {
        TextsMut {
            iter: self.children.iter_mut(),
        }
    }

    /// Appends a child node to the `Element`, returning the appended node.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let mut elem = Element::bare("root", "ns1");
    ///
    /// assert_eq!(elem.children().count(), 0);
    ///
    /// elem.append_child(Element::bare("child", "ns1"));
    ///
    /// {
    ///     let mut iter = elem.children();
    ///     assert_eq!(iter.next().unwrap().name(), "child");
    ///     assert_eq!(iter.next(), None);
    /// }
    ///
    /// let child = elem.append_child(Element::bare("new", "ns1"));
    ///
    /// assert_eq!(child.name(), "new");
    /// ```
    pub fn append_child(&mut self, child: Element) -> &mut Element {
        self.children.push(Node::Element(child));
        if let Node::Element(ref mut cld) = *self.children.last_mut().unwrap() {
            cld
        } else {
            unreachable!()
        }
    }

    /// Appends a text node to an `Element`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let mut elem = Element::bare("node", "ns1");
    ///
    /// assert_eq!(elem.text(), "");
    ///
    /// elem.append_text_node("text");
    ///
    /// assert_eq!(elem.text(), "text");
    /// ```
    pub fn append_text_node<S: Into<String>>(&mut self, child: S) {
        self.children.push(Node::Text(child.into()));
    }

    /// Appends a string as plain text to an `Element`.
    ///
    /// If the last child node of the element is a text node, the string will be appended to it.
    /// Otherwise, a new text node will be created.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let mut elem = Element::bare("node", "ns1");
    ///
    /// assert_eq!(elem.text(), "");
    ///
    /// elem.append_text_node("text");
    ///
    /// elem.append_text(" and more text");
    ///
    /// assert_eq!(elem.nodes().count(), 1);
    /// ```
    pub fn append_text<S: Into<String>>(&mut self, text: S) {
        if let Some(Node::Text(ref mut child)) = self.children.last_mut() {
            child.push_str(&text.into());
        } else {
            self.append_text_node(text);
        }
    }

    /// Appends a node to an `Element`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, Node};
    ///
    /// let mut elem = Element::bare("node", "ns1");
    ///
    /// elem.append_node(Node::Text("hello".to_owned()));
    ///
    /// assert_eq!(elem.text(), "hello");
    /// ```
    pub fn append_node(&mut self, node: Node) {
        self.children.push(node);
    }

    /// Returns the concatenation of all text nodes in the `Element`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::Element;
    ///
    /// let elem: Element = "<node xmlns=\"ns1\">hello,<split /> world!</node>".parse().unwrap();
    ///
    /// assert_eq!(elem.text(), "hello, world!");
    /// ```
    #[must_use]
    pub fn text(&self) -> String {
        self.texts().fold(String::new(), |ret, new| ret + new)
    }

    /// Returns a reference to the first child element with the specific name and namespace, if it
    /// exists in the direct descendants of this `Element`, else returns `None`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, NSChoice};
    ///
    /// let elem: Element = r#"<node xmlns="ns"><a/><a xmlns="other_ns" /><b/></node>"#.parse().unwrap();
    /// assert!(elem.get_child("a", "ns").unwrap().is("a", "ns"));
    /// assert!(elem.get_child("a", "other_ns").unwrap().is("a", "other_ns"));
    /// assert!(elem.get_child("b", "ns").unwrap().is("b", "ns"));
    /// assert_eq!(elem.get_child("c", "ns"), None);
    /// assert_eq!(elem.get_child("b", "other_ns"), None);
    /// assert_eq!(elem.get_child("a", "inexistent_ns"), None);
    /// ```
    pub fn get_child<'a, N: AsRef<str>, NS: Into<NSChoice<'a>>>(
        &self,
        name: N,
        namespace: NS,
    ) -> Option<&Element> {
        let namespace = namespace.into();
        for fork in &self.children {
            if let Node::Element(ref e) = *fork {
                if e.is(name.as_ref(), namespace) {
                    return Some(e);
                }
            }
        }
        None
    }

    /// Returns a mutable reference to the first child element with the specific name and namespace,
    /// if it exists in the direct descendants of this `Element`, else returns `None`.
    pub fn get_child_mut<'a, N: AsRef<str>, NS: Into<NSChoice<'a>>>(
        &mut self,
        name: N,
        namespace: NS,
    ) -> Option<&mut Element> {
        let namespace = namespace.into();
        for fork in &mut self.children {
            if let Node::Element(ref mut e) = *fork {
                if e.is(name.as_ref(), namespace) {
                    return Some(e);
                }
            }
        }
        None
    }

    /// Returns whether a specific child with this name and namespace exists in the direct
    /// descendants of the `Element`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, NSChoice};
    ///
    /// let elem: Element = r#"<node xmlns="ns"><a /><a xmlns="other_ns" /><b /></node>"#.parse().unwrap();
    /// assert_eq!(elem.has_child("a", "other_ns"), true);
    /// assert_eq!(elem.has_child("a", "ns"), true);
    /// assert_eq!(elem.has_child("a", "inexistent_ns"), false);
    /// assert_eq!(elem.has_child("b", "ns"), true);
    /// assert_eq!(elem.has_child("b", "other_ns"), false);
    /// assert_eq!(elem.has_child("b", "inexistent_ns"), false);
    /// ```
    pub fn has_child<'a, N: AsRef<str>, NS: Into<NSChoice<'a>>>(
        &self,
        name: N,
        namespace: NS,
    ) -> bool {
        self.get_child(name, namespace).is_some()
    }

    /// Removes the first child with this name and namespace, if it exists, and returns an
    /// `Option<Element>` containing this child if it succeeds.
    /// Returns `None` if no child matches this name and namespace.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use minidom::{Element, NSChoice};
    ///
    /// let mut elem: Element = r#"<node xmlns="ns"><a /><a xmlns="other_ns" /><b /></node>"#.parse().unwrap();
    /// assert!(elem.remove_child("a", "ns").unwrap().is("a", "ns"));
    /// assert!(elem.remove_child("a", "ns").is_none());
    /// assert!(elem.remove_child("inexistent", "inexistent").is_none());
    /// ```
    pub fn remove_child<'a, N: AsRef<str>, NS: Into<NSChoice<'a>>>(
        &mut self,
        name: N,
        namespace: NS,
    ) -> Option<Element> {
        let name = name.as_ref();
        let namespace = namespace.into();
        let idx = self.children.iter().position(|x| {
            if let Node::Element(ref elm) = x {
                elm.is(name, namespace)
            } else {
                false
            }
        })?;
        self.children.remove(idx).into_element()
    }

    /// Remove the leading nodes up to the first child element and
    /// return it
    pub fn unshift_child(&mut self) -> Option<Element> {
        while !self.children.is_empty() {
            if let Some(el) = self.children.remove(0).into_element() {
                return Some(el);
            }
        }

        None
    }
}

/// An iterator over references to child elements of an `Element`.
pub struct Children<'a> {
    iter: slice::Iter<'a, Node>,
}

impl<'a> Iterator for Children<'a> {
    type Item = &'a Element;

    fn next(&mut self) -> Option<&'a Element> {
        for item in &mut self.iter {
            if let Node::Element(ref child) = *item {
                return Some(child);
            }
        }
        None
    }
}

/// An iterator over mutable references to child elements of an `Element`.
pub struct ChildrenMut<'a> {
    iter: slice::IterMut<'a, Node>,
}

impl<'a> Iterator for ChildrenMut<'a> {
    type Item = &'a mut Element;

    fn next(&mut self) -> Option<&'a mut Element> {
        for item in &mut self.iter {
            if let Node::Element(ref mut child) = *item {
                return Some(child);
            }
        }
        None
    }
}

/// An iterator over references to child elements of an `Element`.
pub struct ContentsAsChildren<'a> {
    iter: alloc::vec::Drain<'a, Node>,
}

impl Iterator for ContentsAsChildren<'_> {
    type Item = Element;

    fn next(&mut self) -> Option<Element> {
        for item in &mut self.iter {
            if let Node::Element(child) = item {
                return Some(child);
            }
        }
        None
    }
}

/// An iterator over references to child text nodes of an `Element`.
pub struct Texts<'a> {
    iter: slice::Iter<'a, Node>,
}

impl<'a> Iterator for Texts<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        for item in &mut self.iter {
            if let Node::Text(ref child) = *item {
                return Some(child);
            }
        }
        None
    }
}

/// An iterator over mutable references to child text nodes of an `Element`.
pub struct TextsMut<'a> {
    iter: slice::IterMut<'a, Node>,
}

impl<'a> Iterator for TextsMut<'a> {
    type Item = &'a mut String;

    fn next(&mut self) -> Option<&'a mut String> {
        for item in &mut self.iter {
            if let Node::Text(ref mut child) = *item {
                return Some(child);
            }
        }
        None
    }
}

/// An iterator over references to all child nodes of an `Element`.
pub type Nodes<'a> = slice::Iter<'a, Node>;

/// An iterator over mutable references to all child nodes of an `Element`.
pub type NodesMut<'a> = slice::IterMut<'a, Node>;

/// A builder for `Element`s.
pub struct ElementBuilder {
    root: Element,
}

impl ElementBuilder {
    /// Sets a custom prefix. It is not possible to set the same prefix twice.
    pub fn prefix<S: Into<Namespace>>(
        mut self,
        prefix: Prefix,
        namespace: S,
    ) -> Result<ElementBuilder> {
        if self.root.prefixes.get(&prefix).is_some() {
            return Err(Error::DuplicatePrefix);
        }
        self.root.prefixes.insert(prefix, namespace.into());
        Ok(self)
    }

    /// Sets an attribute.
    #[must_use]
    pub fn attr<V: IntoAttributeValue>(mut self, name: NcName, value: V) -> ElementBuilder {
        self.root.set_attr(RxmlNamespace::NONE, name, value);
        self
    }

    /// Sets an attribute.
    #[must_use]
    pub fn attr_ns<V: IntoAttributeValue>(
        mut self,
        ns: RxmlNamespace,
        name: NcName,
        value: V,
    ) -> ElementBuilder {
        self.root.set_attr(ns, name, value);
        self
    }

    /// Appends anything implementing `Into<Node>` into the tree.
    #[must_use]
    pub fn append<T: Into<Node>>(mut self, node: T) -> ElementBuilder {
        self.root.append_node(node.into());
        self
    }

    /// Appends an iterator of things implementing `Into<Node>` into the tree.
    #[must_use]
    pub fn append_all<T: Into<Node>, I: IntoIterator<Item = T>>(
        mut self,
        iter: I,
    ) -> ElementBuilder {
        for node in iter {
            self.root.append_node(node.into());
        }
        self
    }

    /// Builds the `Element`.
    #[must_use]
    pub fn build(self) -> Element {
        self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxml::xml_ncname;
    use std::collections::BTreeMap;

    #[test]
    fn test_element_new() {
        let mut attrs = AttrMap::new();
        attrs.insert(
            String::from("namespace").into(),
            xml_ncname!("name").to_owned(),
            "value".to_string(),
        );
        let elem = Element::new(
            "name".to_owned(),
            "namespace".to_owned(),
            (None, "namespace".to_owned()),
            attrs,
            Vec::new(),
        );

        assert_eq!(elem.name(), "name");
        assert_eq!(elem.ns(), "namespace".to_owned());
        assert_eq!(elem.attr_ns("namespace", "name"), Some("value"));
        assert_eq!(elem.attr("inexistent"), None);
    }

    #[test]
    fn test_from_reader_simple() {
        let xml = b"<foo xmlns='ns1'></foo>";
        let elem = Element::from_reader(&xml[..]);

        let elem2 = Element::builder("foo", "ns1").build();

        assert_eq!(elem.unwrap(), elem2);
    }

    #[test]
    fn test_from_reader_nested() {
        let xml = b"<foo xmlns='ns1'><bar xmlns='ns1' baz='qxx' /></foo>";
        let elem = Element::from_reader(&xml[..]);

        let nested = Element::builder("bar", "ns1")
            .attr(xml_ncname!("baz").to_owned(), "qxx")
            .build();
        let elem2 = Element::builder("foo", "ns1").append(nested).build();

        assert_eq!(elem.unwrap(), elem2);
    }

    #[test]
    fn test_from_reader_with_prefix() {
        let xml = b"<foo xmlns='ns1'><prefix:bar xmlns:prefix='ns1' baz='qxx' /></foo>";
        let elem = Element::from_reader(&xml[..]);

        let nested = Element::builder("bar", "ns1")
            .attr(xml_ncname!("baz").to_owned(), "qxx")
            .build();
        let elem2 = Element::builder("foo", "ns1").append(nested).build();

        assert_eq!(elem.unwrap(), elem2);
    }

    #[test]
    fn test_from_reader_split_prefix() {
        let xml = b"<foo:bar xmlns:foo='ns1'/>";
        let elem = Element::from_reader(&xml[..]).unwrap();

        assert_eq!(elem.name(), String::from("bar"));
        assert_eq!(elem.ns(), String::from("ns1"));
        // Ensure the prefix is properly added to the store
        assert_eq!(
            elem.prefixes.get(&Some(String::from("foo"))),
            Some(&String::from("ns1"))
        );
    }

    #[test]
    fn parses_spectest_xml() {
        // From: https://gitlab.com/lumi/minidom-rs/issues/8
        let xml = br#"<rng:grammar xmlns:rng="http://relaxng.org/ns/structure/1.0">
                <rng:name xmlns:rng="http://relaxng.org/ns/structure/1.0"></rng:name>
            </rng:grammar>
        "#;
        let _ = Element::from_reader(&xml[..]).unwrap();
    }

    #[test]
    fn does_not_unescape_cdata() {
        let xml = b"<test xmlns='test'><![CDATA[&apos;&gt;blah<blah>]]></test>";
        let elem = Element::from_reader(&xml[..]).unwrap();
        assert_eq!(elem.text(), "&apos;&gt;blah<blah>");
    }

    #[test]
    fn test_compare_all_ns() {
        let xml = b"<foo xmlns='foo' xmlns:bar='baz'><bar:meh xmlns:bar='baz' /></foo>";
        let elem = Element::from_reader(&xml[..]).unwrap();

        let elem2 = elem.clone();

        let xml3 = b"<foo xmlns='foo'><bar:meh xmlns:bar='baz'/></foo>";
        let elem3 = Element::from_reader(&xml3[..]).unwrap();

        let xml4 = b"<prefix:foo xmlns:prefix='foo'><bar:meh xmlns:bar='baz'/></prefix:foo>";
        let elem4 = Element::from_reader(&xml4[..]).unwrap();

        assert_eq!(elem, elem2);
        assert_eq!(elem, elem3);
        assert_eq!(elem, elem4);
    }

    #[test]
    fn test_compare_empty_children() {
        let elem1 = Element::bare("p", "");
        let elem2 = Element::builder("p", "")
            .append(Node::Element(Element::bare("span", "")))
            .build();

        assert_ne!(elem1, elem2);
    }

    #[test]
    fn test_from_reader_with_prefixes() {
        let xml = b"<foo><bar xmlns='baz'/></foo>";
        let elem =
            Element::from_reader_with_prefixes(&xml[..], String::from("jabber:client")).unwrap();

        let xml2 = b"<foo xmlns='jabber:client'><bar xmlns='baz'/></foo>";
        let elem2 = Element::from_reader(&xml2[..]).unwrap();

        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_from_reader_with_prefixes_serialization() {
        let prefixes: BTreeMap<Option<String>, String> = {
            let mut tmp = BTreeMap::new();
            tmp.insert(None, String::from("foo"));
            tmp.insert(Some(String::from("test")), String::from("bar"));
            tmp
        };

        let input = r#"<foo test:attr="true"><bar/></foo>"#;
        let output = r#"<foo xmlns='foo' xmlns:test='bar' test:attr='true'><bar/></foo>"#;

        let elem = Element::from_reader_with_prefixes(input.as_ref(), prefixes).unwrap();
        assert_eq!(String::from(&elem), output);
    }

    #[test]
    fn failure_with_duplicate_namespace() {
        let _: Element = r###"<?xml version="1.0" encoding="UTF-8"?>
            <wsdl:definitions
                    xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/"
                    xmlns:xsd="http://www.w3.org/2001/XMLSchema">
                <wsdl:types>
                    <xsd:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                    </xsd:schema>
                </wsdl:types>
            </wsdl:definitions>
        "###
        .parse()
        .unwrap();
    }
}
