# Local patches

This directory is based on the crates.io `minidom` 0.18.0 release. The
upstream crate source checksum in the pre-vendoring `server/Cargo.lock` was
`040b4e38f8abd85ddf9ea00646accc716b9d15e283b0aacb16f697ddba432c97`.

Local difference:

- `src/element.rs`: add `Element::ns_ref()` to expose the already stored
  namespace as a borrowed `&str`. The dark account-resource model uses this
  to preflight untrusted XML tree sizes without allocating a clone of a large
  namespace string.
- `src/element.rs`: replace the derived recursive `Element::clone()` with an
  iterative tree clone. It preserves element names, namespaces, prefix maps,
  attributes, and ordered text/element children without using call-stack depth
  proportional to XML nesting. The dark account-resource model invokes this
  through snapshot clones during updates and successful resumption.
- `src/element.rs`: add iterative `Element::drop()` so releasing a retained or
  temporary payload also uses heap-backed traversal instead of recursive
  destruction. Child lists are taken before each child element is dropped.
- `src/tests.rs`: add a small-stack regression for deep and sibling-heavy
  iterative element cloning.

All other crate files are unchanged from the upstream release.
