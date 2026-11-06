Version NEXT:

Version 0.18, released 2025-10-28:
  * Breaking:
    * Remove 'std' feature for now because it makes build fail on
      no-default-features.
    * Add `Element::attr_ns` that requires the attribute namespace in addition
      to `Element::attr` (which defaults to `rxml::Namespace::none()`. Similar
      changes on `ElementBuilder` methods.
      `Element` now uses `AttrMap` to store attributes, and this also implies
      some more changes in the Element interface such as iterators on
      attributes.
  * Changes
    * Use thiserror for the error type. (!616)

Version 0.17, released 2025-09-17:
  * Changes
    * Almost make the whole crate `no_std`, only `std::io` is still remaining.
    * Update rxml dependency to 0.13.
  * Fixes
    * Remove warnings for elided lifetimes (rustc 1.90)
    * Fixes serialization and ensure prefixes are set on Element when using
      `Element::from_reader_with_prefixes`.

Version 0.16, released 2024-07-23:
  * Breaking
    * Element comparison returns unequal when number of nodes is unequal.
  * Changes
    * New `Element::append_text` method.
    * Update rxml to 0.11.0
  * Fixes
    * Prevent splitting of text nodes in TreeBuilder when there is no element in between.
    * Update to edition 2021.
    * Fix clippy lints, and other compiler warnings.

Version 0.15.2, released 2023-05-13:
  * Changes
    * Fix a memory corruption on closing tags for elements with a name longer
      than 24 bytes
    * Only enable the mt features of rxml, we don’t need any additional one

Version 0.15.1, released 2023-01-15:
  * Changes
    * Add `Element::from_reader_with_prefixes`
    * (#44) Add test ensuring parsing two namespaces resolv

Version 0.15.0, released 2022-07-13:
  * Changes
    * Drop quick-xml dependency (astro1, jssfr)

Version 0.14.0, released 2022-03-07:
  * Changes
    * Bump quick-xml dependency (thanks eijebong!)
  * Fixes
    * Handle identical namespaces of sibling elements correctly (thanks Jasper!)
    * Fix support for newer rustc (see https://github.com/rust-lang/rust/issues/90199)

Version 0.13.0, released 2021-01-13:
  * Changes
    * Force namespaces on Element, which was a breaking change.

Version 0.12.1, released 2021-01-13, yanked:
  * Changes
    * Bump quick-xml dependency.

Version 0.12, released 2020-02-15:
  * Breaking
    * `Element.write_to` doesn't prepand xml prelude anymore. Use `write_to_decl` when necessary.
    * PartialEq implementation for Element and Node have been changed to
      ensure namespaces match even if the objects are not structurally
      equivalent in Rust.
  * Changes
    * Explicitely focus on XMPP. Some features will eventually be removed from
      the project to comply with this.
    * Update edition to 2018
    * Add NSChoice enum to allow comparing NSs differently
    * Add impl for From<Into<Element>> for Node
  * Fixes
    * Update old CI configuration with newer Rust images

Version 0.11.1, released 2019-09-06:
  * Changes
    * Update to quick-xml 0.16
    * Add a default "comments" feature to transform comments into errors when unset.

Version 0.11.0, released 2019-06-14:
  * Breaking
    * Get rid of IntoElements, replace with `Into<Node>` and `<T: Into<Node> IntoIterator<Item = T>>`
  * Fixes
    * Remote unused `mut` attribute on variable
  * Changes
    * Update quick-xml to 0.14
    * Split Node into its own module
    * Nicer Debug implementation for NamespaceSet

Version 0.10.0, released 2018-10-21:
  * Changes
    * Update quick-xml to 0.13
    * Update doc to reflect switch from xml-rs to quick-xml.

Version 0.9.1, released 2018-05-29:
  * Fixes
    * Lumi fixed CDATA handling, minidom will not unescape CDATA bodies anymore.
  * Small changes
    - Link Mauve implemented IntoAttributeValue on std::net::IpAddr.

Version 0.9.0, released 2018-04-10:
  * Small changes
    - Upgrade quick_xml to 0.12.1

Version 0.8.0, released 2018-02-18:
  * Additions
    - Link Mauve replaced error\_chain with failure ( https://gitlab.com/lumi/minidom-rs/merge_requests/27 )
    - Yue Liu added support for writing comments and made the writing methods use quick-xml's EventWriter ( https://gitlab.com/lumi/minidom-rs/merge_requests/26 )

Version 0.6.2, released 2017-08-27:
  * Additions
    - Link Mauve added an implementation of IntoElements for all Into<Element> ( https://gitlab.com/lumi/minidom-rs/merge_requests/19 )

Version 0.6.1, released 2017-08-20:
  * Additions
    - Astro added Element::has_ns, which checks whether an element's namespace matches the passed argument. ( https://gitlab.com/lumi/minidom-rs/merge_requests/16 )
    - Link Mauve updated the quick-xml dependency to the latest version.
  * Fixes
    - Because break value is now stable, Link Mauve rewrote some code marked FIXME to use it.

Version 0.6.0, released 2017-08-13:
  * Big changes
    - Astro added proper support for namespace prefixes. ( https://gitlab.com/lumi/minidom-rs/merge_requests/14 )
  * Fixes
    - Astro fixed a regression that caused the writer not to escape its xml output properly. ( https://gitlab.com/lumi/minidom-rs/merge_requests/15 )

Version 0.5.0, released 2017-06-10:
  * Big changes
    - Eijebong made parsing a lot faster by switching the crate from xml-rs to quick_xml. ( https://gitlab.com/lumi/minidom-rs/merge_requests/11 )
