function subscribeToEvents() {
  console.debug('Attempting to subscribe to events...');

  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const endpoint = `${protocol}${window.location.host}/api/events`;

  const ws = new WebSocket(endpoint);

  ws.onopen = () => {
    console.debug('Opened socket');
  };

  ws.onmessage = (evt) => {
    console.debug('Got event', evt);

    const payload = JSON.parse(evt.data);
    const eventType = payload['event'];

    switch (eventType) {
      case 'simple_message':
        bulmaToast.toast({
          message: payload['message'],
          type: 'is-info'
        });

        break;

      case 'loading_state_change':
        const accountID = payload['account_id'];
        const loadingState = payload['loading_state'];

        const stateElem = document.querySelector(`section[data-account-id="${accountID}"] #account-loading-state`);
        if (stateElem && loadingState === 'Loading Complete') {
          window.location.reload();
        } else if (stateElem) {
          stateElem.textContent = loadingState;
        }

        break;

      case 'similar_image':
        const mediaID = payload['media_id'];
        if (document.querySelector(`section[data-media-id="${mediaID}"]`)) {
          window.location.reload();
          return;
        }

        const link = payload['link'];
        const linkElement = document.createElement('a');
        linkElement.href = link;
        linkElement.target = '_blank';
        linkElement.textContent = link;

        const text = document.createTextNode('A similar image was found: ');

        const content = document.createElement('span');
        content.appendChild(text);
        content.appendChild(linkElement);

        bulmaToast.toast({
          message: content,
          type: 'is-info',
          closeOnClick: false,
          pauseOnHover: true,
        });

        break;
    }
  };

  ws.onclose = () => {
    console.debug('Socket closed');
    setTimeout(subscribeToEvents, 1000);
  };

  ws.onerror = (err) => {
    console.warn('Socket error', err);
    ws.close();
  };
}

subscribeToEvents();

const rtf = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

const units = {
  year: 24 * 60 * 60 * 1000 * 365,
  month: 24 * 60 * 60 * 1000 * 365 / 12,
  day: 24 * 60 * 60 * 1000,
  hour: 60 * 60 * 1000,
  minute: 60 * 1000,
  second: 1000,
};

function getRelativeTime(toDate, fromDate = new Date()) {
  const elapsed = toDate - fromDate;

  for (const unit in units) {
    if (Math.abs(elapsed) > units[unit] || unit === 'second') {
      return rtf.format(Math.round(elapsed / units[unit]), unit);
    }
  }
}

function updateRelativeTimes() {
  [...document.querySelectorAll('.relative-time[data-timestamp]')].forEach((elem) => {
    const timestamp = parseInt(elem.dataset.timestamp, 10);
    const date = new Date(timestamp * 1000);

    elem.textContent = getRelativeTime(date);
  });

  setTimeout(updateRelativeTimes, 1000 * 15);
}

updateRelativeTimes();
