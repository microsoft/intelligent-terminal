# BYOK API key storage security review

Last updated: 2026-08-14

Status: Draft for Security review

## 1. Review request

Intelligent Terminal allows a user to configure an OpenAI-compatible model
provider for a supported built-in agent. Some providers require a long-lived
API key.

This review requests approval to use Windows Credential Manager generic
credentials as the version 1 storage mechanism for those API keys, subject to
the controls and release criteria in this document.

The proposed decision is:

1. Store the API key with `CredWriteW` as a `CRED_TYPE_GENERIC` credential
   scoped to the current Windows user and local computer.
2. Persist only an opaque credential identifier in `settings.json`.
3. Resolve the key only when launching an agent configured for the selected
   provider.
4. Accept that Credential Manager does not isolate generic credentials from
   other processes running as the same Windows user.
5. Treat stronger app-only isolation as a future design option unless Security
   requires it for the initial release.

Security approval is requested for the storage decision, the stated security
boundary, and the residual risk acceptance. Approval of this document does not
waive the release criteria in section 10.

## 2. Scope

### In scope

- Collection of an API key in the Intelligent Terminal Settings UI.
- Storage, lookup, replacement, and deletion of the key.
- Data persisted in `settings.json`.
- API key exposure through logs, telemetry, crash dumps, and diagnostics.
- Isolation from other Windows users and other processes running as the same
  user.
- Upgrade, uninstall, provider deletion, and partial-failure behavior.
- Packaged Store, packaged development, and unpackaged development builds.

### Related but reviewed separately

The selected agent requires a process-visible credential. The current agent
adapters provide it through an agent-specific environment variable. The
resulting agent-process and descendant-process exposure is documented in
`byok-byom-agent-support.md`.

This document does not claim that Credential Manager protects the key after it
has been resolved into process memory. The runtime delivery path must remain
covered by the product threat model and the supported agent's security review.

### Out of scope security boundaries

The following actors can obtain secrets available to the interactive user and
are not security boundaries for this feature:

- Local administrator or SYSTEM.
- A debugger or code-injection capability against Intelligent Terminal or WTA.
- A fully compromised Windows account.
- Kernel compromise.

These exclusions do not exclude ordinary untrusted code running as the user.
The same-user Generic Credential risk is explicitly assessed in section 7.

## 3. Security objectives

### Required objectives

1. The API key is never persisted in plaintext in `settings.json`, generated
   agent configuration, Intelligent Terminal logs, telemetry, or diagnostics.
2. A different Windows user cannot retrieve the API key through supported
   Credential Manager APIs.
3. The key remains local to the machine and does not roam with Terminal
   settings.
4. A missing or unreadable required credential fails closed. The agent must not
   silently fall back to a different paid or hosted provider.
5. Provider deletion, replacement, reset, and uninstall have defined
   credential cleanup behavior.
6. Error messages identify the recovery action without including the key.
7. Tests demonstrate the data flow and negative cases described in section 10.

### Non-objectives

1. Protecting a key from an administrator, SYSTEM, a debugger, or a compromised
   user account.
2. App-only isolation from every process running as the same Windows user. The
   selected Credential Manager API provides a user boundary, not an application
   boundary.
3. Preventing the selected agent from accessing the key. The agent needs the
   credential to authenticate to the configured provider.

## 4. Proposed design

### 4.1 Storage

Intelligent Terminal creates a random GUID for each stored API key and writes a
Windows Generic Credential with:

| Field | Value |
|---|---|
| Type | `CRED_TYPE_GENERIC` |
| Target name | `IntelligentTerminal.LocalModelProvider/<credential-id>` |
| Credential blob | UTF-8 API key bytes |
| Persistence | `CRED_PERSIST_LOCAL_MACHINE` |
| Display user name | `Intelligent Terminal` |

`CRED_PERSIST_LOCAL_MACHINE` means the credential persists for the current user
on the local computer across logon sessions. It does not make the credential
available to every user on the computer, and it does not roam to another
computer.

The credential ID is an object reference, not an authorization secret.
Security does not depend on the GUID being unguessable because a same-user
process may enumerate Generic Credentials.

Implementation:

- `src/cascadia/inc/CustomModelCredential.h`
  - `StoreApiKey`
  - `RemoveApiKey`
- `tools/wta/src/custom_model_provider.rs`
  - `read_api_key`

### 4.2 Settings persistence

The custom provider record stores non-secret metadata and an opaque reference:

```json
{
    "id": "<provider-id>",
    "name": "<display-name>",
    "baseUrl": "https://provider.example/v1",
    "apiContract": "openai-compatible",
    "apiKeyCredential": "{credential-guid}",
    "apiKeyRequired": true,
    "models": [
        {
            "id": "<model-id>",
            "name": "<model-name>"
        }
    ]
}
```

The API key is not serialized into settings. `apiKeyRequired` records user
intent independently of whether credential lookup succeeds, allowing WTA to
fail closed when a referenced credential is missing.

Implementation:

- `src/cascadia/TerminalSettingsModel/CustomModelProvider.cpp`
- `src/cascadia/inc/CustomModelProviderUtils.h`

### 4.3 Runtime lookup

The Terminal process passes only provider metadata to WTA:

- Provider base URL.
- Model identifier.
- Credential identifier.
- Whether an API key is required.

WTA reads the credential immediately before configuring a supported agent. It
does not resolve the key for unsupported agents or cloud model discovery.
Missing required credentials produce an actionable error asking the user to
re-enter the API key.

The Rust reader copies the credential blob, validates UTF-8, trims surrounding
whitespace, rejects an empty credential, and clears the initial byte buffer
after conversion. This clearing reduces incidental retention but does not claim
complete process-memory erasure; additional string and environment copies exist
while the agent is running.

Implementation:

- `src/cascadia/TerminalApp/TerminalPage.cpp`
  - `_BuildSharedWtaEnvironment`
- `tools/wta/src/custom_model_provider.rs`
  - `Config::resolve_api_key`
  - `configure_child`
  - `scrub_child_for_cloud_discovery`

### 4.4 Creation and rollback

The Settings UI:

1. Trims the API key and treats a whitespace-only value as no key.
2. Writes the credential before committing the provider record.
3. Deletes the newly written credential if the settings commit throws.
4. Clears the API key field after save or cancel.

Implementation:

- `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp`
  - `AddCustomModelProvider`
  - `CancelCustomModelProvider`

## 5. Data flow

```text
User
  |
  | enters key
  v
Settings PasswordBox
  |
  | CredWriteW(CRED_TYPE_GENERIC)
  v
Windows Credential Manager
  |                                   settings.json
  | key blob                          | provider metadata
  | keyed by opaque GUID              | opaque credential ID only
  +---------------------+-------------+
                        |
                        | selected provider launch
                        v
                    Terminal
                        |
                        | base URL, model, credential ID, required flag
                        v
                       WTA
                        |
                        | CredReadW immediately before agent launch
                        v
              Supported selected agent process
```

The cleartext key exists at these points:

- Settings UI memory while entered.
- Temporary C++ memory during `CredWriteW`.
- Credential Manager's protected credential store.
- Temporary WTA memory after `CredReadW`.
- The selected agent's process-visible credential configuration.

It must not appear in settings, command-line arguments, generated configuration
files, logs, telemetry, or user-facing error text.

## 6. Why Credential Manager is the proposed version 1 choice

Credential Manager is preferred over application-managed encryption for the
initial release because it:

- Uses a Windows-supported credential storage API intended for passwords and
  authentication material.
- Avoids inventing key derivation, key storage, encryption, integrity, and
  migration formats.
- Protects data at rest using the current user's Windows credential boundary.
- Keeps the secret out of Terminal's JSON settings and settings backup flows.
- Provides direct create, read, and delete operations with no plaintext file.
- Works for the existing Win32/C++ and Rust packaged process architecture.
- Supports keyless local providers without creating a dummy credential.

This choice is not justified by secrecy of the target name and does not claim
package or application isolation.

## 7. Threat analysis

| ID | Threat | Existing or required mitigation | Residual risk | Disposition |
|---|---|---|---|---|
| T1 | API key is written to `settings.json` | Persist only a random credential ID and `apiKeyRequired` | A settings reader learns provider metadata and credential target ID | Mitigated |
| T2 | Another Windows user reads the key | Credential is stored in the current user's credential set | Administrators are outside the boundary | Mitigated within scope |
| T3 | A same-user process reads the Generic Credential | No app ACL exists for Win32 Generic Credentials; target prefix and GUID only prevent accidental collision | A same-user process can use Credential APIs to enumerate/read the key | Explicit risk acceptance required |
| T4 | Key roams to another device | Use `CRED_PERSIST_LOCAL_MACHINE`, not enterprise persistence | Settings may roam without the credential; launch then fails closed | Mitigated |
| T5 | Required key is missing or deleted externally | Persist `apiKeyRequired`; reject missing/empty credential | User must re-enter the key | Mitigated |
| T6 | Key appears in logs, telemetry, diagnostics, or errors | Never log credential blobs or credential-bearing environment values; redact relevant environment keys | Third-party agent diagnostics require separate validation | Release evidence required |
| T7 | Key appears in crash or memory dumps | Do not intentionally include secrets in diagnostics; document dump sensitivity; minimize lifetime and clear temporary buffers where practical | Full-memory dumps may contain process-visible secrets | Accepted for authenticated local secret handling, subject to dump-policy review |
| T8 | Provider deletion leaves an orphaned credential | Delete credential before losing its ID; retain retry state or reconcile credentials when deletion fails | Current best-effort deletion can orphan a credential | Must fix before release |
| T9 | Crash occurs after credential write but before settings commit | Synchronous rollback handles ordinary exceptions | Process termination can bypass rollback | Reconciliation/reset mechanism required or risk acceptance required |
| T10 | Upgrade or uninstall leaves credentials behind | Define stable target prefix; provide remove-all/reset and uninstall behavior | MSIX uninstall behavior alone is not assumed to delete Generic Credentials | Release design required |
| T11 | Dev, Store, and unpackaged builds collide | Include product channel/package identity in the resource namespace, or document intentional sharing | Current fixed prefix can share credentials across variants for the same user | Must resolve before release |
| T12 | Malformed or oversized key causes corruption | Enforce `CRED_MAX_CREDENTIAL_BLOB_SIZE`, valid UTF-8, and non-empty input | Provider-specific key syntax is not validated | Mitigated |
| T13 | Product silently changes provider after lookup failure | Treat a missing required key as an error; no cloud fallback | User experiences a failed launch | Mitigated |
| T14 | Credential replacement loses both old and new key | Write new credential first; delete the old credential only after new write succeeds | Cleanup failure can leave the old credential | Prefer availability; reconcile orphan later |

### 7.1 Same-user access decision

Microsoft's Generic Credential APIs isolate credentials by user, not by
application. Any process running with the user's credential access can attempt
to read a Generic Credential when it knows or enumerates its target.

The product must not describe this design as "only Intelligent Terminal can
read the API key." The accurate claim is:

> The API key is stored by Windows Credential Manager for the current Windows
> user and local computer. It is not stored in Intelligent Terminal settings.

Approval of Credential Manager therefore requires Security to accept the
Windows user account as the version 1 storage boundary. If app-only isolation
is required, section 9 contains stronger alternatives.

## 8. Required compensating controls

The following controls are part of the proposed Credential Manager design:

1. **No plaintext persistence.** Automated tests inspect serialized provider
   settings and generated agent configuration for a sentinel key.
2. **No command-line secret.** The API key is never passed as a process
   argument.
3. **No secret logging.** Intelligent Terminal and WTA log credential IDs and
   state transitions only when needed; they never log the credential value.
4. **Fail closed.** A provider marked `apiKeyRequired` cannot launch without a
   non-empty readable credential.
5. **Least distribution.** Only the selected supported agent receives the
   resolved key. Unsupported agents and cloud discovery receive neither the
   shared metadata nor the secret.
6. **Bounded input.** Reject values larger than
   `CRED_MAX_CREDENTIAL_BLOB_SIZE`; reject empty stored credentials.
7. **Rollback.** Delete a newly created credential when committing settings
   fails.
8. **Reliable deletion.** Do not permanently discard the credential ID when
   `CredDeleteW` fails. Retain a tombstone/retry record or implement safe
   reconciliation.
9. **Reset and cleanup.** Provide a user-visible way to remove all stored BYOK
   credentials and define uninstall behavior.
10. **Namespace separation.** Separate Store, development package, and
    unpackaged credential namespaces unless cross-channel sharing is explicitly
    approved.
11. **Error hygiene.** Errors may include the provider name and credential ID
    but not the key or a key-derived substring.
12. **Documentation.** User documentation states that the key is stored in
    Windows Credential Manager under the signed-in Windows account.

## 9. Alternative designs

### 9.1 Windows Credential Locker (`PasswordVault`)

**Benefit:** WinRT API intended for credential storage and convenient for
AppContainer applications.

**Limitation for Intelligent Terminal:** Microsoft documents that AppContainer
apps can access only their own locker, while non-AppContainer desktop apps can
access all lockers for the current user. Intelligent Terminal's full-trust
desktop process does not gain app-only isolation merely by replacing
`CredWriteW` with `PasswordVault`.

**Decision:** Not preferred as a drop-in replacement. It changes API surface
without solving the same-user isolation requirement for the current process
model.

### 9.2 User-scoped DPAPI (`CryptProtectData`)

**Benefit:** Simple Windows-supported authenticated encryption for data stored
by the application.

**Limitation:** User-scoped DPAPI uses substantially the same user trust
boundary. Another process running as the user can decrypt a blob if it can
access the blob and any required entropy. Shipping static "optional entropy"
with the application does not create an application security boundary.

**Decision:** Not preferred. It adds ciphertext file lifecycle, ACL, migration,
and corruption handling without materially improving the selected threat
boundary.

### 9.3 TPM-backed CNG key with package-SID ACL

**Design:** Create a non-exportable key with the Microsoft Platform Crypto
Provider, restrict access to the intended package SID, and use it to wrap a
data-encryption key or encrypt the API key.

**Benefits:**

- Can provide stronger resistance to raw credential-store access.
- Can bind use to device-backed key material.
- Can potentially distinguish Store and development package identities.

**Costs and open questions:**

- Requires careful validation that every legitimate reader, including packaged
  WTA, carries the expected package identity and SID.
- Requires a separate development story for unpackaged builds.
- Requires key rotation, corruption recovery, migration, backup, and reset
  behavior.
- Package-SID ACL and KSP behavior must be penetration-tested; administrator
  access remains outside the boundary.

**Decision:** Strong candidate if Security requires package-bound storage
without user interaction.

### 9.4 AppContainer credential broker

**Design:** Move credential storage and provider authentication into a small
AppContainer broker. Terminal and WTA communicate with it through authenticated,
ACL-restricted IPC. The broker uses its app-scoped Credential Locker and does
not return the long-lived key when it can perform the authenticated operation
on behalf of the caller.

**Benefits:**

- Establishes a real application security principal distinct from ordinary
  same-user desktop processes.
- Reduces direct access to the stored key.
- Centralizes auditing, rate limiting, key rotation, and cleanup.

**Costs and open questions:**

- New process, IPC contract, lifecycle, deployment, and servicing surface.
- Caller authorization must prevent another same-user process from using the
  broker as an oracle.
- If the agent still requires the raw key, returning it over IPC reduces the
  value of the broker.

**Decision:** Strongest local architectural option, but too large for a simple
version 1 storage substitution.

### 9.5 Windows Hello gated decryption

**Design:** Require Windows Hello user verification before unwrapping or using
the key, either for every operation or once per bounded session.

**Benefits:**

- Adds user presence and makes silent same-user credential theft harder.
- Provides a clear user-visible authorization event.

**Costs:**

- Adds prompts to background agent startup and Autofix.
- Requires cancellation, remote-session, accessibility, and unavailable-Hello
  behavior.
- Does not protect the key after it enters an authorized process.

**Decision:** Consider as an optional high-security mode rather than the
default user experience.

### 9.6 Service-issued short-lived token or local provider proxy

**Design:** Avoid storing the upstream API key in Intelligent Terminal. Exchange
it for a scoped, short-lived token, or keep it in a trusted local/enterprise
proxy that authenticates the user or device.

**Benefits:**

- Reduces value and lifetime of a stolen client credential.
- Enables provider-side revocation, scope, audit, and policy.

**Costs:**

- Requires provider or enterprise service support.
- Not universal for arbitrary OpenAI-compatible endpoints.
- Introduces network and service availability dependencies.

**Decision:** Preferred when supported by the provider, but cannot be the
generic BYOK storage contract.

## 10. Release criteria and evidence

Credential Manager approval should be conditioned on the following evidence:

### Functional and negative tests

- Save a provider with a key; confirm the key is absent from `settings.json`.
- Confirm the expected Generic Credential exists for the current user.
- Launch a supported provider and confirm authentication succeeds.
- Delete the credential externally; confirm launch fails closed with no cloud
  fallback.
- Store an empty or invalid UTF-8 credential; confirm launch fails without
  exposing credential content.
- Configure a keyless local endpoint; confirm no credential is created.
- Delete a provider; confirm its credential is deleted.
- Inject `CredDeleteW` failure; confirm the credential ID remains recoverable
  and cleanup is retried.
- Terminate the process between credential write and settings commit; confirm
  reconciliation or reset can remove the orphan.
- Confirm Store, development, and unpackaged builds follow the approved
  namespace-sharing decision.
- Confirm a second Windows user cannot retrieve the first user's credential.

### Secret scanning

Using a unique sentinel API key:

- Search `settings.json`, application state, generated configuration, logs,
  ETW/telemetry payloads, bug-report output, and ordinary crash metadata.
- Confirm the sentinel is absent from command lines.
- Confirm error paths for missing credentials, malformed credentials, and
  provider failures do not include the sentinel.
- Record any full-memory-dump exposure as expected residual behavior and verify
  dump access/retention policy with Security and Privacy.

### Lifecycle

- Document credential behavior on provider replacement, provider deletion,
  reset, upgrade, repair, package uninstall, and reinstall.
- Provide either deterministic cleanup or an approved residual-retention
  policy.
- Ensure cleanup cannot delete credentials outside Intelligent Terminal's
  approved target namespace.

## 11. Privacy and user experience

- The API key is user-provided authentication data and must be classified
  according to the product privacy and data-handling policy.
- The product must not send the API key to Microsoft telemetry.
- The key is sent only to the endpoint configured by the user through the
  selected agent's provider integration.
- Settings should explain that removing a provider removes its locally stored
  credential.
- Documentation should tell users how to revoke a key at the provider. Local
  deletion cannot revoke a key that was previously disclosed or copied.

## 12. Security review decisions requested

Reviewers are asked to record decisions for the following:

| Question | Proposed answer |
|---|---|
| Is the current Windows user an acceptable v1 at-rest security boundary? | Yes |
| Is `CRED_TYPE_GENERIC` with `CRED_PERSIST_LOCAL_MACHINE` approved for BYOK API keys? | Yes |
| Is same-user Generic Credential readability an accepted residual risk? | Yes, with accurate documentation |
| Must package-bound storage be implemented before initial release? | No |
| Must reliable delete/reconciliation be implemented before release? | Yes |
| Must Store/Dev/unpackaged namespace behavior be resolved before release? | Yes |
| Are full-memory dumps allowed to contain a key used by the process? | Security/Privacy decision required |
| Is agent-process environment delivery covered by this approval? | No; covered by the runtime/agent review |

If Security answers "no" to the first or third question, the recommended next
design is TPM-backed CNG storage with a package-SID ACL, followed by an
AppContainer broker if the CNG design cannot establish the required boundary.

## 13. References

- [Credential Manager `CredWriteW`](https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-credwritew)
- [Credential Manager `CredReadW`](https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-credreadw)
- [`CREDENTIALW` persistence values](https://learn.microsoft.com/windows/win32/api/wincred/ns-wincred-credentialw)
- [Kinds of credentials](https://learn.microsoft.com/windows/win32/secauthn/kinds-of-credentials)
- [Credential Locker for Windows apps](https://learn.microsoft.com/windows/apps/develop/security/credential-locker)
- [`PasswordVault` application boundary](https://learn.microsoft.com/uwp/api/windows.security.credentials.passwordvault)
- [DPAPI `CryptProtectData`](https://learn.microsoft.com/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)
- [CNG DPAPI `NCryptProtectSecret`](https://learn.microsoft.com/windows/win32/api/ncryptprotect/nf-ncryptprotect-ncryptprotectsecret)
- [Windows Hello for Business overview](https://learn.microsoft.com/windows/security/identity-protection/hello-for-business/)
- [Intelligent Terminal security model](../security-model.md)
- [BYOK/BYOM support across built-in agents](byok-byom-agent-support.md)
