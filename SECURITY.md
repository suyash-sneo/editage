# Security policy

## Reporting a vulnerability

Please report security problems privately. Use GitHub's private
vulnerability reporting on the repository: open the **Security** tab and
choose **Report a vulnerability**. This creates a private advisory that only
the maintainers can see.

Please do not report security problems in public issues, pull requests or
discussions.

A useful report includes:

- what you did, what you expected, and what happened;
- the Editage version or commit, and the macOS version;
- whether the problem needs a particular file, folder, volume type or sync
  client to reproduce;
- if a file is needed, a file containing only dummy content, and its
  passphrase. Please never send real secrets.

There is no email address for reports and no bug bounty. We will
acknowledge a report through the advisory, keep you informed while a fix is
prepared, and credit you in the advisory unless you prefer not to be named.

## Scope

In scope: anything in this repository that could make Editage behave
differently from what [docs/security-model.md](docs/security-model.md) and
[docs/save-protocol.md](docs/save-protocol.md) describe. For example:

- plaintext document content or a passphrase written to a file, a log, the
  diagnostics history or the preferences store;
- a save that can damage or lose the previous encrypted file before the
  atomic replacement;
- a save that overwrites a file which changed on disk without the user being
  told;
- a file that Editage writes but the reference age implementation cannot
  decrypt, or a file that is not what the Security Inspector says it is;
- text in the Security Inspector or in failure messages that claims something
  the code does not do;
- a malicious input file that causes memory or time use beyond the documented
  limits, a crash, or incorrect decryption;
- the clipboard being cleared when it holds content from another application.

Out of scope: the risks that
[docs/security-model.md](docs/security-model.md#what-this-application-does-not-protect-against)
lists as not protected against, such as a compromised operating system,
malware that can read process memory, keyloggers, screen capture, other
processes reading the clipboard, and weak passphrases. Weaknesses in the age
format or in the `age` crate itself should be reported to those projects; we
are glad to hear about them too, so we can respond.

The design goals and non-goals are in
[docs/threat-model.md](docs/threat-model.md). Please read it before
reporting, so that we share an understanding of what the application is meant
to defend against.

## Fixes

Every confirmed security bug is fixed together with a regression test that
fails without the fix. The test stays in the test suite permanently.
