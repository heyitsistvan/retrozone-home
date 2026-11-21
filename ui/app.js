/*
 * RetroZone Home's window. It draws the program's state as the program sends it (`state`), and
 * asks the program to do things (its commands, main.rs). It keeps nothing of its own.
 */
'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const view = document.getElementById('view');

let shown = null;
let editingSite = false;
let busy = false;

const escape = text => String(text ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const number = n => Number(n || 0).toLocaleString('en-US');
const host = site => String(site || '').replace(/^https?:\/\//, '');
const lastPart = path => String(path).replace(/[\\/]+$/, '').split(/[\\/]/).pop() || path;

/** A moment, as somebody would say how long ago it was */
function ago(seconds) {
	const since = Math.max(0, Math.floor(Date.now() / 1000) - seconds);
	if (since < 60) return 'just now';
	if (since < 3600) return Math.floor(since / 60) + ' min ago';
	if (since < 86400) return Math.floor(since / 3600) + ' h ago';
	return Math.floor(since / 86400) + ' d ago';
}

// ---- what is drawn ----------------------------------------------------------

function signedOut(state) {
	return `
		<div class="hello">
			<div class="hello__art"><svg><use href="#pad"></use></svg></div>
			<h1>Play your games<br>on every device</h1>
			<p>Your phone, your laptop, any browser signed in to RetroZone can play the games on this computer, straight from here.</p>
			${state.notice ? `<div class="notice">${escape(state.notice)}</div>` : ''}
			<button type="button" class="btn btn--primary btn--big" data-act="sign-in" ${busy ? 'disabled' : ''}>${busy ? '<span class="spinner"></span> Opening…' : 'Sign in with RetroZone'}</button>
			<div class="hello__site">
				${editingSite ? `
					<form data-form="site"><input name="site" value="${escape(state.site)}" spellcheck="false" aria-label="RetroZone address"><button class="btn" type="submit">Use</button></form>
				` : `RetroZone at ${escape(host(state.site))} · <button type="button" class="link link--muted" data-act="edit-site">Change</button>`}
			</div>
		</div>`;
}

function signingIn(state) {
	return `
		<div class="hello">
			<div class="hello__art"><svg><use href="#pad"></use></svg></div>
			<h1>Finish in your browser</h1>
			<p>Your browser opened RetroZone. Allow this computer there, and check that it shows the same code:</p>
			<div class="code">${escape(state.signIn.code)}</div>
			<div class="waiting"><span class="spinner"></span> Waiting for you to allow it…</div>
			<div class="row">
				<button type="button" class="btn" data-act="open-sign-in">Open the page again</button>
				<button type="button" class="btn btn--quiet" data-act="cancel-sign-in">Cancel</button>
			</div>
		</div>`;
}

function noFolders(state) {
	return `
		<div class="hello">
			<div class="hello__art"><svg><use href="#folder"></use></svg></div>
			<h1>Where are your games?</h1>
			<p>Choose the folders your games are in. They are only read: nothing is moved, changed or uploaded.</p>
			${state.notice ? `<div class="notice">${escape(state.notice)}</div>` : ''}
			<button type="button" class="btn btn--primary btn--big" data-act="choose-folder"><svg><use href="#plus"></use></svg> Choose a folder</button>
			<div class="hello__site">You can also drop a folder on this window.</div>
		</div>`;
}

function statusCard(state) {
	const cards = {
		ready: {
			tone: 'good',
			title: 'Ready to play',
			text: `Open RetroZone on any of your devices: the games in these folders come from this computer.`,
		},
		reading: {
			tone: 'busy',
			title: 'Getting your games ready',
			text: state.files === 0
				? `Reading your games for the first time${state.reading ? ' · ' + number(state.reading) + ' so far' : ''}. Only this once.`
				: `Reading new games${state.reading ? ' · ' + number(state.reading) + ' so far' : ''}…`,
			bar: true,
		},
		starting: { tone: 'busy', title: 'Connecting…', text: 'Reaching RetroZone.', bar: true },
		offline: { tone: 'warn', title: "Can't reach RetroZone", text: 'Check the internet connection. It keeps trying by itself.' },
	};
	const card = cards[state.status] ?? cards.starting;

	return `
		<div class="status" data-tone="${card.tone}">
			<span class="status__dot"></span>
			<div class="status__main">
				<h2>${card.title}</h2>
				<p>${card.text}</p>
				${card.bar ? '<div class="status__bar"><i></i></div>' : ''}
			</div>
		</div>`;
}

function running(state) {
	const folders = state.folders.map(folder => `
		<li class="item">
			<svg><use href="#folder"></use></svg>
			<div class="item__main">
				<div class="item__name">${escape(lastPart(folder))}</div>
				<div class="item__sub" title="${escape(folder)}">${escape(folder)}</div>
			</div>
			<div class="item__actions">
				<button type="button" class="icon-btn" data-act="show-folder" data-path="${escape(folder)}" title="Show in Explorer" aria-label="Show in Explorer"><svg><use href="#out"></use></svg></button>
				<button type="button" class="icon-btn" data-act="remove-folder" data-path="${escape(folder)}" title="Stop following this folder" aria-label="Stop following this folder"><svg><use href="#x"></use></svg></button>
			</div>
		</li>`).join('');

	const played = state.activity.length ? `
		<div class="section">
			<div class="section__head"><span class="section__label">Played from here</span></div>
			<ul class="list">
				${state.activity.map(game => `
					<li class="item">
						<svg><use href="#device"></use></svg>
						<div class="item__main">
							<div class="item__name">${escape(game.title)}</div>
							<div class="item__sub">${game.device ? escape(game.device) + ' · ' : ''}<span data-ago="${game.at}">${ago(game.at)}</span></div>
						</div>
					</li>`).join('')}
			</ul>
		</div>` : '';

	return `
		${state.notice ? `<div class="notice">${escape(state.notice)}</div>` : ''}
		${statusCard(state)}
		<div class="section">
			<div class="section__head">
				<span class="section__label">Your games folders</span>
				${state.files ? `<span class="section__label">${number(state.files)} files</span>` : ''}
			</div>
			<ul class="list">${folders}</ul>
			<button type="button" class="add" data-act="choose-folder"><svg><use href="#plus"></use></svg> Add a folder</button>
		</div>
		${played}`;
}

function footer() {
	return `
			<label class="switch" title="Start in the tray when you sign in to Windows"><input type="checkbox" data-act="autostart" ${autostart ? 'checked' : ''}> Start with Windows</label>
			<div class="foot__links">
				<button type="button" class="link link--muted" data-act="open-site">Open RetroZone</button>
				<button type="button" class="link link--muted" data-act="sign-out">Sign out</button>
			</div>`;
}

let autostart = false;

function render(state) {
	shown = state;
	const account = document.getElementById('account');
	account.hidden = !state.account;
	account.textContent = state.account ?? '';
	account.title = state.account ? `Playing from ${state.account}'s library on ${host(state.site)}` : '';

	// What has focus is put back after the redraw, for keyboard use
	const focused = document.activeElement?.dataset?.act;
	const foot = document.getElementById('foot');
	foot.hidden = !state.account;
	foot.innerHTML = state.account ? footer() : '';
	view.innerHTML = {
		'signed-out': signedOut,
		'signing-in': signingIn,
		'no-folders': noFolders,
	}[state.status]?.(state) ?? running(state);
	if (focused) {
		document.querySelector(`[data-act="${focused}"]`)?.focus();
	}
	view.querySelector('[data-form="site"] input')?.focus();
}

// ---- what is done -----------------------------------------------------------

/** Asks the player something in the window itself; resolves to whether they said yes */
function ask(title, text, yes) {
	const sheet = document.getElementById('ask');
	document.getElementById('ask-title').textContent = title;
	document.getElementById('ask-text').textContent = text;
	sheet.querySelector('[data-ask="yes"]').textContent = yes;
	sheet.hidden = false;
	sheet.querySelector('[data-ask="no"]').focus();

	return new Promise(resolve => {
		const done = answer => {
			sheet.hidden = true;
			sheet.onclick = null;
			document.removeEventListener('keydown', onKey);
			resolve(answer);
		};
		const onKey = event => event.key === 'Escape' && done(false);
		document.addEventListener('keydown', onKey);
		sheet.onclick = event => {
			const button = event.target.closest('[data-ask]');
			if (button || event.target === sheet) {
				done(button?.dataset.ask === 'yes');
			}
		};
	});
}

async function act(name, element) {
	try {
		switch (name) {
			case 'sign-in':
				busy = true;
				render(shown);
				await invoke('sign_in', { site: shown.site });
				break;
			case 'open-sign-in': await invoke('open_sign_in'); break;
			case 'cancel-sign-in': await invoke('cancel_sign_in'); break;
			case 'edit-site': editingSite = true; render(shown); break;
			case 'choose-folder': await invoke('choose_folder'); break;
			case 'remove-folder': await invoke('remove_folder', { path: element.dataset.path }); break;
			case 'show-folder': await invoke('show_folder', { path: element.dataset.path }); break;
			case 'open-site': await invoke('open_site'); break;
			case 'sign-out':
				if (await ask('Sign out?', 'Your devices stop playing from this computer until you sign in again. Your games are not touched.', 'Sign out')) {
					await invoke('sign_out');
				}
				break;
			case 'autostart':
				autostart = await invoke('set_autostart', { on: element.checked });
				element.checked = autostart;
				break;
			case 'hide': document.getElementById('tray-note').hidden = true; await invoke('hide'); break;
			case 'quit': await invoke('quit'); break;
		}
	} catch (error) {
		shown = { ...shown, notice: String(error) };
	} finally {
		if (name === 'sign-in') {
			busy = false;
		}
		if (shown && name !== 'edit-site') {
			render(shown);
		}
	}
}

document.addEventListener('click', event => {
	const target = event.target.closest('[data-act]');
	if (target && target.type !== 'checkbox') {
		act(target.dataset.act, target);
	}
});
document.addEventListener('change', event => {
	if (event.target.dataset?.act === 'autostart') {
		act('autostart', event.target);
	}
});
document.addEventListener('submit', event => {
	if (event.target.dataset.form === 'site') {
		event.preventDefault();
		const site = event.target.elements.site.value.trim();
		editingSite = false;
		render({ ...shown, site: site || shown.site });
	}
});
document.addEventListener('keydown', event => {
	if (event.key === 'Escape' && editingSite) {
		editingSite = false;
		render(shown);
	}
});

// A folder dropped on the window is followed like one chosen
listen('tauri://drag-enter', () => document.body.classList.add('drop'));
listen('tauri://drag-leave', () => document.body.classList.remove('drop'));
listen('tauri://drag-drop', async event => {
	document.body.classList.remove('drop');
	if (!shown?.account) {
		return;
	}
	for (const path of event.payload?.paths ?? []) {
		await invoke('add_folder', { path }).catch(error => render({ ...shown, notice: String(error) }));
	}
});

listen('state', event => {
	// A site address being typed in is kept until it is used
	const site = editingSite || shown?.status === 'signed-out' ? shown?.site : null;
	render(site && event.payload.status === 'signed-out' ? { ...event.payload, site } : event.payload);
});
listen('close-requested', () => {
	document.getElementById('tray-note').hidden = false;
	document.querySelector('#tray-note [data-act="hide"]').focus();
});

// "2 min ago" stays true while the window is open
setInterval(() => document.querySelectorAll('[data-ago]').forEach(element => {
	element.textContent = ago(Number(element.dataset.ago));
}), 30000);

(async () => {
	autostart = await invoke('autostart').catch(() => false);
	render(await invoke('state'));
})();
