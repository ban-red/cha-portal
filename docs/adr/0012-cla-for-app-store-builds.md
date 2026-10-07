# 0012: A contributor licence agreement, so Cha Player can ship in app stores

- **Status:** accepted (2026-10-07). Settles the open question in [0010](0010-native-client-macos-first.md) ("a CLA or an exception in our licence, decided before outside contributions arrive").
- **Context:**
  - Cha Portal is AGPL-3.0-or-later. Apple's App Store and Mac App Store add usage terms (device limits, DRM, a ban on redistribution) that the AGPL forbids passing on (§10), so AGPL code ships there only with every copyright holder's permission. Steam, the Microsoft Store, Google Play, Flathub and a notarised download don't need that permission.
  - With [0011](0011-own-gamestream-client.md), the client has no third-party GPL code left: what it borrows is under permissive licences ([PROVENANCE](../PROVENANCE.md)). Today every line of our own code has one copyright holder, the maintainer, so the maintainer can already publish an App Store build. Once outside contributions arrive, that stops being true unless contributors grant the same right.
  - Two ways were considered:
    - **A public App Store exception** (an additional permission under AGPL §7). It lets anyone ship Cha builds in app stores, provided the source stays public, and needs no agreement from contributors beyond the licence they already accept. It also lets anyone put a paid Cha build or clone in the App Store.
    - **A contributor licence agreement.** Contributors keep their copyright and license it to the maintainer broadly, so the maintainer alone can ship store builds. Everyone else is bound by the plain AGPL, so they can't. The cost is a signature from each contributor, which some people won't give.
- **Decision:**
  - **No public exception.** The licence stays plain AGPL-3.0-or-later.
  - **Contributors sign [the CLA](../../CLA.md) once**, before their first pull request is merged. It is a licence, not a copyright assignment: copyright, and a patent licence, on Apache ICLA lines. It is bound by the maintainer's commitments: every distributed version with the contribution is also published under the AGPL with its source; no selling a contribution on its own under non-free terms; the rights pass only to a successor who takes on the same commitments. If the first commitment is broken, the grant falls back to the AGPL.
  - **CLA Assistant Lite** (`.github/workflows/cla.yml`) asks on each new contributor's first pull request and records signatures in `signatures/cla.json` on the `cla-signatures` branch. The maintainer and bots are on its allowlist.
  - **DCO sign-off stays** for every commit: it records where each change came from, commit by commit, which the CLA's one-time signature doesn't.
- **Consequences:**
  - Cha Player can ship in the App Store, and in any other store, with outside contributions in it. Third parties can still sell Cha under the AGPL wherever the AGPL is allowed; only the maintainer can ship it where it isn't.
  - Contributing takes one more step, and some contributors will decline to sign. Trademarking the name, not the licence, is how others are stopped from shipping under the Cha name.
  - The CLA's wording should be reviewed by a lawyer before the first outside pull request is merged.
