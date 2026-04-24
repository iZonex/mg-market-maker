import './lib/tokens.css'
import './lib/utilities.css'
import App from './App.svelte'
import { BRAND } from './lib/branding.js'
import { mount } from 'svelte'

// Stamp the brand name into `<title>` at boot so a forked repo
// picks up its own branding without editing index.html.
document.title = `${BRAND.productName} Dashboard`

const app = mount(App, { target: document.getElementById('app') })

export default app
