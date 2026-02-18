# ADR-0010: AGPL-3.0 License

## Status

Accepted

## Context

Waddle Social is positioned as an open-source alternative to proprietary chat platforms. License choice affects:
- Community contributions and adoption
- Commercial use and modification
- Alignment with project values (openness, user freedom)

We evaluated licenses:
- **MIT/Apache-2.0**: Permissive; allows proprietary forks
- **GPL-3.0**: Copyleft; requires source sharing for distributed software
- **AGPL-3.0**: Network copyleft; requires source sharing for network services
- **BSL/SSPL**: Source-available but not OSI-approved; restricts cloud providers
- **MPL-2.0**: File-level copyleft; middle ground

## Decision

We will license Waddle Social under **AGPL-3.0**.

## Consequences

### Positive

- **Network Copyleft**: Anyone running Waddle as a service must share modifications
- **User Freedom**: Users always have access to the code running their service
- **Contribution Incentive**: Companies benefit more by contributing upstream
- **No Proprietary Forks**: Prevents closed-source commercial competitors
- **Ecosystem Alignment**: Common in self-hosted and federated software

### Negative

- **Corporate Hesitancy**: Some companies avoid AGPL due to compliance concerns
- **Contribution Friction**: Contributors must agree to license terms
- **Dual Licensing Complexity**: If offering commercial license later, must own all code
- **Dependency Constraints**: Cannot use incompatibly-licensed libraries

### Neutral

- **CLA Consideration**: May implement Contributor License Agreement for flexibility

## Implementation Notes

- Include AGPL-3.0 license text in repository root
- Add license headers to source files
- Document compliance requirements for self-hosters
- Consider CLA for future dual-licensing option

## Related

- All source code and documentation falls under this license unless explicitly noted
