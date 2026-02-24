// CarbonPaper Browser Extension - Popup Script

const toggle = document.getElementById('enableToggle');
const statusDot = document.getElementById('statusDot');
const statusText = document.getElementById('statusText');

function updateStatusUI(enabled, connected) {
  toggle.checked = enabled;

  statusDot.className = 'status-dot';
  if (!enabled) {
    statusDot.classList.add('disabled');
    statusText.textContent = 'Disabled';
  } else if (connected) {
    statusDot.classList.add('connected');
    statusText.textContent = 'Connected to CarbonPaper';
  } else {
    statusDot.classList.add('disconnected');
    statusText.textContent = 'CarbonPaper not running';
  }
}

// Get current status
chrome.runtime.sendMessage({ type: 'getStatus' }, (response) => {
  if (response) {
    updateStatusUI(response.enabled, response.connected);
  }
});

// Toggle handler
toggle.addEventListener('change', () => {
  chrome.runtime.sendMessage(
    { type: 'setEnabled', enabled: toggle.checked },
    (response) => {
      if (response) {
        // Re-check status after a brief delay for connection
        setTimeout(() => {
          chrome.runtime.sendMessage({ type: 'getStatus' }, (status) => {
            if (status) {
              updateStatusUI(status.enabled, status.connected);
            }
          });
        }, 1000);
      }
    }
  );
});
