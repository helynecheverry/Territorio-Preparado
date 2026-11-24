'use strict';
// Trabajador de servicio de la aplicación ciudadana.
//
// Hace tres cosas, y las tres existen porque en una emergencia la red se cae:
//
// 1. Recibe los avisos de Web Push y los muestra, incluso con la aplicación
//    cerrada. Sin esto, todo el backend de alertas no llega a nadie.
// 2. Guarda el contenido educativo para servirlo sin red.
// 3. Guarda el armazón de la aplicación, para que abrirla sin datos muestre algo
//    útil en lugar del error del navegador.
//
// Lo que deliberadamente **no** guarda en caché es cualquier otra respuesta de la
// API. Servir una recomendación de albergue vieja sería peor que no servir nada:
// mandaría a alguien a un sitio elegido con datos de otro momento.

const SHELL = 'tp-shell-v1';
const CONTENT = 'tp-content-v1';
const BUNDLE = '/api/v1/lessons/bundle';

// Lo mínimo para que la aplicación abra sin red y pueda mostrar el contenido
// guardado. No incluye nada que dependa de datos vivos.
const SHELL_FILES = [
  '/app/alerta.html',
  '/app/alerta.js',
  '/app/alerta.css',
  '/app/icon.svg',
  '/app/manifest.webmanifest',
];

self.addEventListener('install', event => {
  event.waitUntil(
    caches
      .open(SHELL)
      .then(cache => cache.addAll(SHELL_FILES))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', event => {
  event.waitUntil(
    caches
      .keys()
      .then(names =>
        Promise.all(
          names.filter(n => n !== SHELL && n !== CONTENT).map(n => caches.delete(n))
        )
      )
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', event => {
  const request = event.request;
  if (request.method !== 'GET') return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  // El contenido educativo: red primero, y si no hay red, lo guardado. Es el
  // único recurso de la API que se guarda, porque es el único cuya versión de
  // ayer sigue siendo correcta hoy.
  if (url.pathname === BUNDLE) {
    event.respondWith(
      fetch(request)
        .then(response => {
          if (response.ok) caches.open(CONTENT).then(c => c.put(BUNDLE, response.clone()));
          return response;
        })
        .catch(() => caches.match(BUNDLE).then(hit => hit || Response.error()))
    );
    return;
  }

  // Cualquier otra ruta de la API pasa directa. Una recomendación de albergue o
  // un aviso guardados serían información de otro momento presentada como actual.
  if (url.pathname.startsWith('/api/')) return;

  // El armazón: lo guardado primero para que abra al instante y sin red, y se
  // refresca en segundo plano.
  event.respondWith(
    caches.match(request).then(hit => {
      const fresh = fetch(request)
        .then(response => {
          if (response.ok) caches.open(SHELL).then(c => c.put(request, response.clone()));
          return response;
        })
        .catch(() => hit || Response.error());
      return hit || fresh;
    })
  );
});

/// Antigüedad a partir de la cual se advierte de que el aviso llegó tarde.
///
/// Los servicios de push retienen mensajes cuando el dispositivo está sin
/// conexión, así que un aviso puede aparecer mucho después. El texto dice «hace N
/// min» calculado al enviarlo, de modo que entregado tarde mentiría. No se oculta
/// —puede seguir siendo útil— pero se marca.
const LATE_MS = 60 * 60 * 1000;

self.addEventListener('push', event => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch (e) {
    data = {};
  }
  const level = data.level || 'info';
  const sent = data.sent_at ? Date.parse(data.sent_at) : NaN;
  const late = Number.isFinite(sent) && Date.now() - sent > LATE_MS;
  const body = late
    ? 'Recibido con retraso. ' + (data.body || '')
    : data.body || 'Abre la aplicación para ver el detalle.';

  event.waitUntil(
    self.registration.showNotification(data.title || 'Territorio Preparado', {
      body,
      icon: '/app/icon.svg',
      badge: '/app/icon.svg',
      lang: 'es',
      // Una etiqueta por nivel: así un aviso informativo posterior no reemplaza
      // en la bandeja a uno de evacuación que la persona todavía no ha leído.
      tag: 'tp-' + level,
      // Una evacuación no se descarta sola. El resto sí, para no acumular.
      requireInteraction: level === 'evacuate',
      silent: false,
      vibrate: level === 'evacuate' ? [300, 100, 300, 100, 300] : [200],
      data: {
        url: data.url || '/app/alerta.html',
        hazard: data.hazard_type || 'earthquake',
        level,
      },
    })
  );
});

self.addEventListener('notificationclick', event => {
  event.notification.close();
  const info = event.notification.data || {};
  // Se abre directamente en la lección de esa amenaza, que ya está guardada en
  // el dispositivo: en ese momento puede no haber red para ir a buscarla.
  const target = '/app/alerta.html#amenaza=' + encodeURIComponent(info.hazard || 'earthquake');
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then(clients => {
      const open = clients.find(c => c.url.includes('/app/alerta.html'));
      if (open) {
        open.focus();
        return open.navigate(target).catch(() => undefined);
      }
      return self.clients.openWindow(target);
    })
  );
});

// El navegador rota la suscripción de push por su cuenta. Si eso ocurre, el
// registro del servidor queda apuntando a un extremo muerto, así que se vuelve a
// registrar. La aplicación lo repite al abrirse, y esto cubre el caso de que no
// se abra en mucho tiempo.
self.addEventListener('pushsubscriptionchange', event => {
  event.waitUntil(
    self.registration.pushManager
      .getSubscription()
      .then(subscription => {
        if (!subscription) return undefined;
        const keys = subscription.toJSON().keys || {};
        return fetch('/api/v1/push-devices', {
          method: 'POST',
          credentials: 'same-origin',
          headers: {
            'Content-Type': 'application/json',
            Origin: self.location.origin,
          },
          body: JSON.stringify({
            endpoint: subscription.endpoint,
            p256dh: keys.p256dh,
            auth: keys.auth,
            label: 'navegador',
          }),
        });
      })
      .catch(() => undefined)
  );
});
