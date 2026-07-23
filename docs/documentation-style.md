# Documentation standard

QsoRipper uses ASD-STE100 Simplified Technical English, Issue 9, for
documentation and reports.

The user can request a different style. Use that style only for the requested
content.

## Scope

Use this standard for these types of text:

- README files
- Architecture, design, API, integration, and operation documents
- Test, experiment, incident, and investigation reports
- Release notes
- Issue and pull request text
- Review findings, plans, and generated summaries

This standard governs all documentation. Apply it to new text and to text that
you change in an existing file.

## Vocabulary

- Use approved words only with their approved meanings and parts of speech.
- Use established QsoRipper terms as technical nouns or technical verbs.
- Define an uncommon project term at its first use.
- Use one term for one item or action.
- Do not use a different term only to add variety.
- Use American English spelling.
- Do not use Latin abbreviations.
- Keep a multi-word noun to three words or fewer.

## Controlled technical terms

Use these QsoRipper terms as technical nouns:

- ADIF
- API
- backend
- callsign
- client
- decoder
- engine
- endpoint
- gRPC
- logbook
- protobuf
- QSO
- QRZ
- RPC
- TUI

Use these QsoRipper terms as technical verbs:

- decode
- deserialize
- export
- import
- parse
- serialize
- stream
- sync

Add a term to this list when it has a special project meaning. Use that term
with the same meaning in all documents.

Identifiers and product names can contain other technical terms. Keep these
items exact.

## Verbs and voice

- Use the infinitive, imperative, simple present, simple past, or simple future.
- Use a past participle only as an adjective when the context is clear.
- Use an `-ing` word only as a technical noun or technical modifier.
- Do not use an auxiliary verb to make a complex verb construction.
- Use active voice when you know the agent.
- You can use passive voice in descriptive text when you do not know the agent.
- Do not use contractions.

## Sentences

- Put only one instruction in each sentence.
- You can put more than one instruction in a sentence when the actions occur
  at the same time.
- Use no more than 20 words in a procedural sentence.
- Use no more than 25 words in a descriptive sentence.
- Use no more than six sentences in one paragraph.
- Use a vertical list for complex text.

## Punctuation

- Do not use a semicolon.
- Use a hyphen when Issue 9 requires one.
- Do not use punctuation to join long sentences.
- Keep punctuation in exact technical content unchanged.

## Exact technical content

Keep these items exact:

- Code and identifiers
- Commands and file paths
- Protocol fields and wire values
- Configuration keys
- Log text and test output
- Direct quotations
- Legal notices

Explain an exact item with STE text when an explanation is necessary.

## Review

Review new and changed prose against the Issue 9 rules and dictionary.
Record project-specific terms as technical nouns or technical verbs.

Check each technical term before you add it to the controlled list.
Automated checks do not replace the dictionary and context review.

Do not state that text conforms to STE unless you complete the full review.

Reference:
[ASD-STE100 Issue 9](https://www.asd-ste100.org/assets/files/ASD-STE100_ISSUE9.pdf).
