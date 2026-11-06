// Copyright (c) 2020 lumi <lumi@pew.im>
// Copyright (c) 2020 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2020 Bastien Orivel <eijebong+minidom@bananium.fr>
// Copyright (c) 2020 Astro <astro@spaceboyz.net>
// Copyright (c) 2020 Maxime “pep” Buquet <pep@bouah.net>
// Copyright (c) 2020 Matt Bilker <me@mbilker.us>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Provides an error type for this crate.

use std::io;

use thiserror::Error;

/// Our main error type.
#[derive(Debug, Error)]
pub enum Error {
    /// Error from rxml parsing or writing
    #[error("XML error: {0}")]
    XmlError(#[from] rxml::Error),

    /// I/O error from accessing the source or destination.
    ///
    /// Even though the [`rxml`] crate emits its errors through
    /// [`io::Error`] when using it with [`BufRead`][`io::BufRead`],
    /// any rxml errors will still be reported through the
    /// [`XmlError`][`Self::XmlError`] variant.
    #[error("I/O error: {0}")]
    Io(io::Error),

    /// An error which is returned when the end of the document was reached prematurely.
    #[error("the end of the document has been reached prematurely")]
    EndOfDocument,

    /// An error which is returned when an element being serialized doesn't contain a prefix
    /// (be it None or Some(_)).
    #[error("the prefix is invalid")]
    InvalidPrefix,

    /// An error which is returned when an element doesn't contain a namespace
    #[error("the XML element is missing a namespace")]
    MissingNamespace,

    /// An error which is returned when a prefixed is defined twice
    #[error("the prefix is already defined")]
    DuplicatePrefix,
}

impl From<io::Error> for Error {
    fn from(other: io::Error) -> Self {
        match other.downcast::<rxml::Error>() {
            Ok(e) => Self::XmlError(e),
            Err(e) => Self::Io(e),
        }
    }
}

impl From<rxml::strings::Error> for Error {
    fn from(err: rxml::strings::Error) -> Error {
        rxml::error::Error::from(err).into()
    }
}

/// Our simplified Result type.
pub type Result<T> = ::core::result::Result<T, Error>;
