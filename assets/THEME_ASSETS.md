# Local Theme Assets

The application runs without these optional assets, falling back to graphite
and the system sans-serif stack. Run from the project directory. `/theme/{name}`
serves only five explicitly allowed files; it is not a filesystem browser.

## Artwork

`assets/island-background.jpg` is the owner's supplied 1023 x 538 JPEG, copied
without modification. SHA-256:
`baa69e1679722587fcada315cf76f0a1e0b878c3964e370b5dd7f5d390466071`.
The filter is CSS, not baked into the image. Local use was requested; public
redistribution rights have not been established. The JPEG is ignored by Git.
For another installation, provide an independently authorized background here.

## Typography

[Kraf Engineering](https://kraf-engineering.com/) uses Satoshi Black (900) for
its wordmark. Promptlist uses the same family and weight, with 400/500/700 for
the interface. [Satoshi](https://www.fontshare.com/fonts/satoshi) is by Indian
Type Foundry, designed by Deni Anggara. Official files were obtained through
[Fontshare's stylesheet](https://api.fontshare.com/v2/css?f[]=satoshi@400,500,700,900&display=swap),
not redistributed from the reference website. Files are unmodified:

- `assets/fonts/satoshi-regular.woff2` (400)
- `assets/fonts/satoshi-medium.woff2` (500)
- `assets/fonts/satoshi-bold.woff2` (700)
- `assets/fonts/satoshi-black.woff2` (900)

Fontshare's [ITF Free Font License](https://www.fontshare.com/licenses/itf-ffl),
version 2.0 dated August 17, 2026, permits self-hosting for one's own website/app
but restricts redistribution through repositories. These binaries are therefore
Git-ignored. Other developers must obtain their own copies from Fontshare under
its terms. No font API or external font request is made by the running app.

The clear header / filtered background treatment references
[Maria Norris Esparza](https://marianorrisesparza.com/). No reference-site image
has been copied into Promptlist.
