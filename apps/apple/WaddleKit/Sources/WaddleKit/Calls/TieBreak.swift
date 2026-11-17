/// XEP-0353 tie-break-1 uses RFC 4790 `i;octet`: unsigned UTF-8 bytes,
/// with the shorter string first when all shared bytes match.
func compareOctetStrings(_ left: String, _ right: String) -> Int {
    for (leftByte, rightByte) in zip(left.utf8, right.utf8) {
        if leftByte != rightByte { return Int(leftByte) - Int(rightByte) }
    }
    return left.utf8.count - right.utf8.count
}

/// The lower session id wins a simultaneous proposal to the same peer.
/// Identical session ids defer to the full JIDs under the same octet order.
func incomingProposeWinsTieBreak(
    incomingSid: String, outgoingSid: String, incomingFrom: JID, ourFullJID: JID
) -> Bool {
    let sidOrder = compareOctetStrings(incomingSid, outgoingSid)
    if sidOrder != 0 { return sidOrder < 0 }
    return compareOctetStrings(incomingFrom.description, ourFullJID.description) < 0
}
