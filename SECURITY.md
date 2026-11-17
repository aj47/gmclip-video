# Security

Report a vulnerability privately through GitHub: the repository's **Security** tab,
**Report a vulnerability**. Please do not open a public issue for it.

gmclip runs agents against transcripts and footage that may carry injected instructions,
so the boundary between what an agent may do and what only the owner may do matters
most. `spec/threat-model.md` describes what gmclip defends against and
`spec/security-requirements.md` lists the requirements. A way for an agent, a project
file or a media file to gain owner authority, read files outside the folders the owner
allowed, or run code is in scope.

There are no supported releases yet; fixes land on `main`.
