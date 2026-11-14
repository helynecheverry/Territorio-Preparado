'use strict';
// Aplicación ciudadana de avisos.
//
// Sin dependencias ni paso de compilación, como el resto de `web/`.
//
// Dos decisiones de criterio que atraviesan todo el archivo:
//
// - El permiso de notificaciones se pide **solo** tras un toque explícito. Los
//   navegadores penalizan pedirlo al cargar, y una persona que lo rechaza por
//   sorpresa es una persona a la que ya no podremos avisar.
// - La guía de qué hacer se guarda en el dispositivo y se muestra aunque no haya
//   red. Todo lo demás —avisos, destinos— se pide en vivo, porque una
//   recomendación de albergue guardada sería información de otro momento.

const $ = s => document.querySelector(s);
let user = null;
let mode = 'login';
let challenge = '';
let coords = null;
let lugares = [];
let bundle = null;
/// Qué puede hacer de verdad este despliegue. La interfaz no ofrece un canal ni
/// un plan que nadie va a poder cumplir: una casilla de SMS sin proveedor
/// contratado es una promesa que se descubre rota durante una emergencia.
let capacidades = { channels: [], billing: false };

const BUNDLE = '/api/v1/lessons/bundle';
const AMENAZAS = { earthquake: 'Sismo', flood: 'Inundación', landslide: 'Deslizamiento', general: 'General' };
const NIVELES = { evacuate: 'Evacúa', prepare: 'Prepárate', info: 'Información' };

function notice(message, error = false) {
  $('#notice').textContent = message || '';
  $('#notice').className = error ? 'error' : '';
}

async function api(path, method = 'GET', body) {
  const r = await fetch('/api/' + path, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  });
  const d = await r.json().catch(() => ({ error: 'Respuesta del servidor no disponible' }));
  if (!r.ok) {
    if (r.status === 401 && user) {
      user = null;
      showAccess();
    }
    throw Error(d.error || 'No se pudo completar la solicitud');
  }
  return d;
}

function element(tag, text, cls) {
  const el = document.createElement(tag);
  if (text !== undefined) el.textContent = text;
  if (cls) el.className = cls;
  return el;
}

async function busy(button, fn) {
  button.disabled = true;
  notice('');
  try {
    await fn();
  } catch (e) {
    notice(e.message, true);
  } finally {
    button.disabled = false;
  }
}

// --- Acceso ----------------------------------------------------------------

function setMode(value) {
  mode = value;
  challenge = '';
  $('#credentials').hidden = false;
  $('#proof').hidden = true;
  $('#access-title').textContent = value === 'register' ? 'Crea tu cuenta' : 'Entra a tu cuenta';
  $('#password').autocomplete = value === 'register' ? 'new-password' : 'current-password';
  $('#access-help').textContent =
    value === 'register'
      ? 'Usa al menos 12 caracteres. Verificarás tu correo antes de entrar.'
      : 'Tu correo y un código de seis dígitos protegen el acceso.';
  const link = element('button', value === 'register' ? 'Ya tengo cuenta' : 'Crear una cuenta');
  link.type = 'button';
  link.addEventListener('click', () => setMode(value === 'register' ? 'login' : 'register'));
  $('#switch-mode').replaceChildren(link);
}

function showAccess() {
  $('#access').hidden = false;
  $('#tabs').hidden = true;
  $('#logout').hidden = true;
  document.querySelectorAll('.tab').forEach(t => (t.hidden = true));
  $('#identity').textContent = 'Territorio Preparado';
  setMode('login');
}

$('#credentials').addEventListener('submit', e => {
  e.preventDefault();
  busy($('#submit-access'), async () => {
    const body = { email: $('#email').value, password: $('#password').value };
    const d = await api(mode === 'register' ? 'auth/register' : 'auth/login', 'POST', body);
    challenge = d.challenge || '';
    if (!challenge) throw Error('No se recibió el desafío de verificación');
    $('#credentials').hidden = true;
    $('#proof').hidden = false;
    notice('Revisa tu correo e introduce el código.');
  });
});

$('#proof').addEventListener('submit', e => {
  e.preventDefault();
  busy($('#proof button.primary'), async () => {
    await api('auth/verify', 'POST', { challenge, code: $('#code').value });
    notice('');
    await start();
  });
});

$('#back').addEventListener('click', () => setMode(mode));

$('#logout').addEventListener('click', () =>
  busy($('#logout'), async () => {
    await api('auth/logout', 'POST', {});
    user = null;
    showAccess();
  })
);

// --- Pestañas --------------------------------------------------------------

function openTab(name) {
  document.querySelectorAll('.tab').forEach(t => (t.hidden = t.id !== 'tab-' + name));
  document
    .querySelectorAll('#tabs button')
    .forEach(b => b.setAttribute('aria-current', b.dataset.tab === name ? 'page' : 'false'));
  if (name === 'guia') renderGuia();
}

document.querySelectorAll('#tabs button').forEach(b =>
  b.addEventListener('click', () => openTab(b.dataset.tab))
);

// --- Notificaciones --------------------------------------------------------

/// La clave VAPID viaja en base64url y `applicationServerKey` espera bytes.
function keyBytes(base64url) {
  const padded = (base64url + '='.repeat((4 - (base64url.length % 4)) % 4))
    .replace(/-/g, '+')
    .replace(/_/g, '/');
  const raw = atob(padded);
  return Uint8Array.from(raw, c => c.charCodeAt(0));
}

async function registration() {
  if (!('serviceWorker' in navigator)) return null;
  return navigator.serviceWorker.register('/app/sw.js', { scope: '/app/' });
}

/// Registra en el servidor la suscripción del navegador.
///
/// Se repite en cada arranque: el navegador rota la suscripción por su cuenta y
/// un registro que apunta a un extremo muerto consume un envío por emergencia.
async function syncPush(subscription) {
  const keys = subscription.toJSON().keys || {};
  await api('v1/push-devices', 'POST', {
    endpoint: subscription.endpoint,
    p256dh: keys.p256dh,
    auth: keys.auth,
    label: 'navegador',
  });
}

/// Oculta lo que este despliegue no puede entregar, diciendo por qué.
function aplicarCapacidades() {
  const haySms = capacidades.channels.includes('sms');
  const etiquetaSms = $('#canal-sms').closest('label');
  etiquetaSms.hidden = !haySms;
  if (!haySms) {
    $('#canal-sms').checked = false;
    $('#telefono-label').hidden = true;
  }
}

async function pushState() {
  const help = $('#push-help');
  const button = $('#enable-push');
  if (!capacidades.channels.includes('push')) {
    help.textContent =
      'Las notificaciones no están configuradas en este despliegue todavía, así que aún no se puede avisar con la aplicación cerrada.';
    button.hidden = true;
    return;
  }
  if (!('serviceWorker' in navigator) || !('PushManager' in window)) {
    help.textContent =
      'Este navegador no admite notificaciones web. Puedes seguir consultando los avisos abriendo la aplicación, y el SMS del plan de pago no depende de esto.';
    button.hidden = true;
    return;
  }
  if (Notification.permission === 'denied') {
    help.textContent =
      'Bloqueaste las notificaciones para este sitio. Hay que reactivarlas desde los ajustes del navegador; desde aquí no se puede volver a pedir.';
    button.hidden = true;
    return;
  }
  const reg = await registration();
  const existing = reg && (await reg.pushManager.getSubscription());
  if (existing) {
    // Resincroniza en silencio: si el extremo cambió, el servidor se actualiza.
    try {
      await syncPush(existing);
      help.textContent = 'Notificaciones activas en este dispositivo.';
      button.hidden = true;
      return;
    } catch (e) {
      help.textContent = 'Las notificaciones están activas pero no se pudo confirmar con el servidor.';
    }
  }
  button.hidden = false;
}

$('#enable-push').addEventListener('click', () =>
  busy($('#enable-push'), async () => {
    const permission = await Notification.requestPermission();
    if (permission !== 'granted') throw Error('Sin permiso de notificaciones no podemos avisarte con la aplicación cerrada.');
    const { public_key: key } = await api('v1/push-public-key');
    const reg = await registration();
    if (!reg) throw Error('Este navegador no admite trabajadores de servicio.');
    const subscription = await reg.pushManager.subscribe({
      // Obligatorio en los navegadores actuales: no se admite push silencioso.
      userVisibleOnly: true,
      applicationServerKey: keyBytes(key),
    });
    await syncPush(subscription);
    notice('Notificaciones activadas en este dispositivo.');
    await pushState();
  })
);

// --- Avisos ----------------------------------------------------------------

async function loadAvisos() {
  const d = await api('v1/alerts/mine');
  const box = $('#avisos');
  box.replaceChildren();
  if (!d.items.length) {
    box.append(element('p', 'Todavía no hay avisos para tus lugares. Eso es lo normal.', 'muted'));
    return;
  }
  for (const a of d.items) {
    const card = element('div', undefined, 'card');
    const aviso = element('div', undefined, 'aviso');
    aviso.dataset.nivel = a.level || 'info';
    aviso.append(element('p', NIVELES[a.level] || 'Información', 'nivel'));
    aviso.append(
      element(
        'p',
        `A ${a.distance_km ?? '—'} km de tu lugar · por ${a.channel === 'sms' ? 'SMS' : 'notificación'}`
      )
    );
    // El estado de entrega se dice tal cual, sin adornarlo: «entregado al
    // servicio» no es «leído», y confundirlos sería engañoso.
    const estado = {
      sent: 'Entregado al servicio de mensajería',
      pending: 'Pendiente de entrega',
      failed: 'No se pudo entregar',
      skipped: 'Descartado',
    }[a.status] || a.status;
    aviso.append(element('p', estado, 'muted'));
    card.append(aviso);
    box.append(card);
  }
}

// --- Lugares ---------------------------------------------------------------

async function loadLugares() {
  const d = await api('v1/subscriptions');
  lugares = d.items;
  const box = $('#lugares');
  box.replaceChildren();
  if (!lugares.length) {
    box.append(element('p', 'Aún no has registrado ningún lugar. Sin un lugar no podemos avisarte.', 'muted'));
  }
  for (const l of lugares) {
    const row = element('div', undefined, 'lugar');
    const info = element('div');
    info.append(element('strong', l.label));
    const sector = [l.neighborhood, l.commune].filter(Boolean).join(' · ') || 'sector sin resolver';
    info.append(element('span', sector, 'muted'));
    info.append(element('div', `Avisos desde: ${NIVELES[l.min_level] || l.min_level}`, 'muted'));
    if (l.phone_last4) info.append(element('div', `SMS a ····${l.phone_last4}`, 'muted'));
    row.append(info);
    const remove = element('button', 'Eliminar', 'danger');
    remove.type = 'button';
    remove.addEventListener('click', () =>
      busy(remove, async () => {
        await api('v1/subscriptions/' + encodeURIComponent(l.id), 'DELETE');
        notice('Lugar eliminado. El dato se borró.');
        await loadLugares();
      })
    );
    row.append(remove);
    box.append(row);
  }
  const select = $('#destino-lugar');
  select.replaceChildren();
  for (const l of lugares) {
    const option = element('option', l.label);
    option.value = l.id;
    select.append(option);
  }
}

$('#canal-sms').addEventListener('change', () => {
  $('#telefono-label').hidden = !$('#canal-sms').checked;
});

$('#usar-ubicacion').addEventListener('click', () =>
  busy($('#usar-ubicacion'), async () => {
    if (!navigator.geolocation) throw Error('Este navegador no puede darnos tu ubicación.');
    coords = await new Promise((resolve, reject) =>
      navigator.geolocation.getCurrentPosition(
        p => resolve({ latitude: p.coords.latitude, longitude: p.coords.longitude, accuracy: p.coords.accuracy }),
        () => reject(Error('No se pudo obtener la ubicación. Revisa el permiso de ubicación del navegador.')),
        { enableHighAccuracy: true, timeout: 15000 }
      )
    );
    // La precisión se muestra porque condiciona el sector resuelto: con 500 m de
    // error, el barrio puede no ser el correcto.
    $('#coords-state').textContent = `Ubicación tomada con precisión de unos ${Math.round(coords.accuracy)} m.`;
  })
);

$('#nuevo-lugar').addEventListener('submit', e => {
  e.preventDefault();
  busy($('#nuevo-lugar button.primary'), async () => {
    if (!coords) throw Error('Primero toma tu ubicación actual.');
    if (!$('#consent').checked) throw Error('Necesitamos tu consentimiento para usar la ubicación.');
    const body = {
      label: $('#lugar-label').value,
      latitude: coords.latitude,
      longitude: coords.longitude,
      channels: { push: $('#canal-push').checked, sms: $('#canal-sms').checked },
      min_level: $('#lugar-nivel').value,
      consent: true,
    };
    if ($('#canal-sms').checked) body.phone = $('#telefono').value.trim();
    const d = await api('v1/subscriptions', 'POST', body);
    const s = d.subscription;
    notice(`Lugar guardado en ${[s.neighborhood, s.commune].filter(Boolean).join(' · ') || 'tu sector'}.`);
    $('#nuevo-lugar').reset();
    $('#telefono-label').hidden = true;
    coords = null;
    $('#coords-state').textContent = 'Necesitamos la ubicación para saber tu sector y buscarte un destino.';
    await loadLugares();
  });
});

// --- A dónde ir ------------------------------------------------------------

$('#buscar-destinos').addEventListener('click', () =>
  busy($('#buscar-destinos'), async () => {
    const id = $('#destino-lugar').value;
    if (!id) throw Error('Registra un lugar primero.');
    const amenaza = $('#destino-amenaza').value;
    const d = await api(
      `v1/subscriptions/${encodeURIComponent(id)}/destinations?hazard=${amenaza}`
    );
    const box = $('#destinos');
    box.replaceChildren();

    // Lo que no se pudo comprobar se dice antes de la lista, no después: es lo
    // que limita la confianza en todo lo que viene debajo.
    if (d.unverified && d.unverified.length) {
      const warn = element('div', undefined, 'card');
      warn.append(
        element(
          'p',
          'Atención: en esta ciudad no se pudo verificar ' +
            d.unverified.map(u => (u === 'flood' ? 'inundación' : 'suelo licuable')).join(' ni ') +
            '. La ausencia de una amenaza no significa que el sitio sea seguro.'
        )
      );
      box.append(warn);
    }

    const list = element('div', undefined, 'card');
    list.append(element('h2', 'Recomendados'));
    if (!d.items.length) {
      list.append(
        element('p', 'No se encontró un espacio apto cerca. Busca una zona despejada, lejos de taludes y del río.', 'muted')
      );
    }
    for (const i of d.items) {
      const row = element('div', undefined, 'destino');
      row.append(element('span', `${Math.round(i.straight_line_km * 1000)} m`, 'dist'));
      const info = element('div');
      info.append(element('strong', i.name));
      info.append(element('div', `${i.neighborhood || ''} · hasta ${i.area_upper_bound_people} personas`, 'muted'));
      row.append(info);
      list.append(row);
    }
    box.append(list);

    // El motivo del descarte es lo que convence a alguien de no quedarse en el
    // parque de al lado. Sin esto, la recomendación parece arbitraria.
    if (d.closer_but_unsuitable && d.closer_but_unsuitable.length) {
      const bad = element('div', undefined, 'card');
      bad.append(element('h2', 'Más cerca, pero no sirven'));
      for (const u of d.closer_but_unsuitable.slice(0, 8)) {
        const row = element('div', undefined, 'descartado');
        const motivos = (u.reasons || [])
          .map(r => (r.requirement === 'liquefaction' ? 'suelo licuable' : 'riesgo alto de inundación'))
          .join(' y ');
        row.append(element('span', `${Math.round(u.straight_line_km * 1000)} m · ${u.name} — `));
        row.append(element('b', motivos));
        bad.append(row);
      }
      box.append(bad);
    }
    const note = element('div', undefined, 'card');
    note.append(element('p', d.meaning, 'muted'));
    box.append(note);
  })
);

// --- Qué hacer -------------------------------------------------------------

/// Trae la guía y deja que el trabajador de servicio la guarde.
///
/// Si no hay red, la respuesta la sirve la copia guardada; por eso un fallo aquí
/// solo se reporta cuando además no hay nada guardado.
async function loadGuia() {
  try {
    bundle = await api('v1/lessons/bundle');
    localStorage.setItem('tp-guia', JSON.stringify(bundle));
  } catch (e) {
    const saved = localStorage.getItem('tp-guia');
    if (!saved) throw e;
    bundle = JSON.parse(saved);
  }
  if ('caches' in window) {
    // Se guarda desde la aplicación y no se confía en que lo haga el trabajador
    // de servicio. En la primera visita el trabajador todavía no controla la
    // página cuando se pide el paquete, así que la copia sin red no existiría
    // hasta la siguiente apertura, y el contenido tiene que estar en el
    // dispositivo **antes** de la emergencia, no después.
    try {
      const cache = await caches.open('tp-content-v1');
      await cache.add(BUNDLE);
    } catch (e) {
      // Sin red y sin copia previa no hay nada que guardar; la insignia lo dirá.
    }
    const stored = await caches.match(BUNDLE).catch(() => null);
    $('#guia-offline').hidden = !stored;
  }
  const select = $('#guia-amenaza');
  const current = select.value;
  select.replaceChildren();
  const tipos = [...new Set((bundle.items || []).map(i => i.hazard_type))];
  for (const t of tipos) {
    const option = element('option', AMENAZAS[t] || t);
    option.value = t;
    select.append(option);
  }
  if (tipos.includes(current)) select.value = current;
}

function renderGuia() {
  const box = $('#guia');
  box.replaceChildren();
  if (!bundle || !(bundle.items || []).length) {
    box.append(
      element('p', 'Todavía no hay contenido publicado. Se publica cuando una persona cualificada lo revisa.', 'muted')
    );
    return;
  }
  const amenaza = $('#guia-amenaza').value;
  const lecciones = bundle.items.filter(i => i.hazard_type === amenaza);
  for (const l of lecciones) {
    const card = element('div', undefined, 'card');
    card.append(element('h2', l.title));
    if (l.summary) card.append(element('p', l.summary, 'muted'));
    for (const [fase, titulo] of [
      ['during', 'Durante'],
      ['after', 'Después'],
      ['before', 'Antes, para prepararse'],
    ]) {
      const pasos = (l.steps || []).filter(s => s.phase === fase);
      if (!pasos.length) continue;
      const block = element('div', undefined, 'fase');
      block.append(element('h4', titulo));
      const ol = element('ol');
      for (const p of pasos) ol.append(element('li', p.text));
      block.append(ol);
      card.append(block);
    }
    if (l.video && l.video.url) {
      // Un archivo de video propio se reproduce aquí mismo y el navegador puede
      // guardarlo; un video de una plataforma se abre fuera, porque incrustarlo
      // metería su rastreo dentro de la aplicación y no funcionaría sin red.
      const esArchivo = /\.(mp4|webm|ogg|ogv|m4v)(\?|#|$)/i.test(l.video.url);
      if (esArchivo) {
        const video = element('video');
        video.src = l.video.url;
        video.controls = true;
        video.preload = 'metadata';
        video.playsInline = true;
        video.className = 'video';
        card.append(video);
      } else {
        const link = element('a', 'Ver el video (se abre fuera de la aplicación)', 'video-link');
        link.href = l.video.url;
        link.target = '_blank';
        link.rel = 'noreferrer noopener';
        card.append(link);
        card.append(
          element('p', 'Necesita conexión. Los pasos de arriba funcionan sin red.', 'muted')
        );
      }
      if (l.video.transcript) {
        const detalle = element('details');
        detalle.append(element('summary', 'Transcripción'));
        detalle.append(element('p', l.video.transcript));
        card.append(detalle);
      }
    }
    // Quién lo dice y quién lo revisó: sin eso, son instrucciones anónimas sobre
    // qué hacer para no morir.
    const fuentes = element('div', undefined, 'fuentes');
    fuentes.append(element('div', 'Fuentes: ' + (l.sources || []).map(s => s.name).join('; ')));
    if (l.reviewed_by) fuentes.append(element('div', 'Revisado por: ' + l.reviewed_by));
    card.append(fuentes);
    box.append(card);
  }
}

$('#guia-amenaza').addEventListener('change', renderGuia);

// --- Plan ------------------------------------------------------------------

async function loadPlan() {
  const [mine, catalog] = await Promise.all([api('v1/billing/me'), api('v1/billing/plans')]);
  const state = $('#plan-state');
  state.replaceChildren();
  state.append(element('h2', mine.plan === 'paid' ? 'Plan de pago' : 'Plan gratuito'));
  if (mine.valid_until) state.append(element('p', 'Vigente hasta ' + mine.valid_until, 'muted'));

  const box = $('#plan-catalog');
  box.replaceChildren();
  for (const p of catalog.items) {
    const card = element('div');
    card.append(element('h2', p.plan === 'paid' ? 'De pago' : 'Gratuito'));
    const precio =
      p.amount_cents > 0
        ? `${(p.amount_cents / 100).toLocaleString('es-CO')} ${p.currency} cada ${p.days} días`
        : 'Sin costo';
    card.append(element('p', precio));
    const ul = element('ul');
    for (const i of p.includes) ul.append(element('li', i));
    card.append(ul);
    box.append(card);
  }
  box.append(element('p', catalog.meaning, 'muted'));

  if (mine.plan === 'paid') return;
  if (!capacidades.billing) {
    box.append(
      element(
        'p',
        'El plan de pago no se puede contratar todavía: falta configurar la pasarela.',
        'muted'
      )
    );
    return;
  }
  // Los términos van antes del cobro: no se contrata sin haber aceptado el texto
  // que dice qué se está vendiendo.
  if (!mine.terms_accepted) {
    try {
      const terms = await api('v1/legal/terms');
      $('#terms-title').textContent = terms.document.title;
      $('#terms-body').textContent = terms.document.body;
      $('#plan-terms').hidden = false;
      $('#do-accept').onclick = () =>
        busy($('#do-accept'), async () => {
          if (!$('#accept-terms').checked) throw Error('Marca la casilla para aceptar.');
          await api('v1/legal/terms/accept', 'POST', { version: terms.document.version });
          $('#plan-terms').hidden = true;
          notice('Términos aceptados.');
          await loadPlan();
        });
    } catch (e) {
      box.append(element('p', 'El plan de pago no está disponible todavía: faltan los términos del servicio.', 'muted'));
    }
    return;
  }
  const contratar = element('button', 'Contratar el plan de pago', 'primary');
  contratar.type = 'button';
  contratar.addEventListener('click', () =>
    busy(contratar, async () => {
      const d = await api('v1/billing/checkout', 'POST', {});
      // La pasarela se abre con esta referencia. El frontend no manipula importes:
      // el servidor los fijó y los vuelve a comprobar al confirmarse el pago.
      notice(`Referencia de pago ${d.reference}. Continúa en la pasarela ${d.provider}.`);
    })
  );
  box.append(contratar);
}

// --- Arranque --------------------------------------------------------------

async function start() {
  user = await api('auth/me');
  capacidades = await api('capabilities').catch(() => capacidades);
  aplicarCapacidades();
  $('#access').hidden = true;
  $('#tabs').hidden = false;
  $('#logout').hidden = false;
  $('#identity').textContent = user.email;
  openTab('avisos');
  await Promise.all([
    loadLugares().catch(e => notice(e.message, true)),
    loadAvisos().catch(() => undefined),
    pushState().catch(() => undefined),
    loadGuia().catch(() => undefined),
    loadPlan().catch(() => undefined),
  ]);
  // Si se llegó desde una notificación, abrir directamente su guía.
  const hash = new URLSearchParams(location.hash.slice(1));
  const amenaza = hash.get('amenaza');
  if (amenaza) {
    if ([...$('#guia-amenaza').options].some(o => o.value === amenaza)) {
      $('#guia-amenaza').value = amenaza;
    }
    openTab('guia');
  }
}

(async () => {
  // El trabajador de servicio se registra siempre, también sin sesión: es lo que
  // permite que un aviso llegue con la aplicación cerrada.
  registration().catch(() => undefined);
  try {
    await start();
  } catch (e) {
    showAccess();
  }
})();
