(function () {
  "use strict";

  const LIMIT = 50;
  const state = {
    roomId: null,
    roomIdentity: null,
    timer: null,
    searching: false,
    draftId: null,
    outboundUsable: false
  };
  const outboundFailureMessages = {
    helper_path_invalid: "The KakaoTalk helper executable failed local security validation.",
    helper_spawn_failed: "The KakaoTalk helper could not be started.",
    helper_timeout: "The KakaoTalk helper timed out while inspecting the app.",
    helper_exit_failed: "The KakaoTalk helper exited before completing the request.",
    helper_output_invalid: "The KakaoTalk helper returned an invalid response.",
    helper_status_unexpected: "The KakaoTalk helper returned an unexpected status.",
    mismatch: "KakaoTalk target validation failed.",
    target_mismatch: "The KakaoTalk target could not be verified.",
    accessibility_permission_unavailable: "Accessibility permission is unavailable for the KakaoTalk helper.",
    target_process_missing: "KakaoTalk did not finish launching in time.",
    target_launch_unavailable: "KakaoTalk could not be launched in the background.",
    target_window_unavailable: "A usable KakaoTalk window is unavailable on the current Space.",
    target_row_missing: "The selected conversation was not found in the KakaoTalk chat list.",
    target_row_ambiguous: "More than one KakaoTalk chat-list row matched the selected conversation.",
    target_row_selection_unavailable: "KakaoTalk could not select the verified chat-list row.",
    target_row_focus_unavailable: "KakaoTalk could not focus the verified chat-list row.",
    target_row_event_unavailable: "KakaoTalk could not open the verified chat-list row.",
    target_row_selection_readback_unavailable: "KakaoTalk could not verify the selected chat-list row after opening it.",
    global_search_unavailable: "KakaoTalk global conversation search is unavailable or could not be verified.",
    global_search_field_missing: "The KakaoTalk global search field did not appear in time.",
    global_search_field_ambiguous: "More than one KakaoTalk global search field was found.",
    global_search_write_unavailable: "The KakaoTalk global search query could not be entered.",
    global_search_readback_mismatch: "The KakaoTalk global search query could not be verified.",
    global_search_result_missing: "The selected conversation was not found in KakaoTalk global search.",
    global_search_result_ambiguous: "More than one KakaoTalk global search result matched the selected conversation.",
    composer_missing: "The selected conversation composer did not appear in time.",
    composer_ambiguous: "More than one enabled KakaoTalk composer was found.",
    composer_write_unavailable: "The KakaoTalk composer could not be updated.",
    composer_readback_mismatch: "The prepared text could not be verified.",
    send_mismatch: "The final send action was not verified.",
    send_unavailable: "The final send action was not verified."
  };
  const elements = {
    refresh: document.getElementById("refresh-button"),
    interval: document.getElementById("refresh-interval"),
    roomCount: document.getElementById("room-count"),
    roomsState: document.getElementById("rooms-state"),
    roomsList: document.getElementById("rooms-list"),
    selectedRoom: document.getElementById("selected-room"),
    sourceState: document.getElementById("source-state"),
    searchForm: document.getElementById("search-form"),
    searchKeyword: document.getElementById("search-keyword"),
    clearSearch: document.getElementById("clear-search"),
    messagesState: document.getElementById("messages-state"),
    messagesList: document.getElementById("messages-list"),
    outboundState: document.getElementById("outbound-state"),
    outboundPreviewForm: document.getElementById("outbound-preview-form"),
    outboundText: document.getElementById("outbound-text"),
    previewButton: document.getElementById("preview-button"),
    outboundPreview: document.getElementById("outbound-preview"),
    previewTarget: document.getElementById("preview-target"),
    previewText: document.getElementById("preview-text"),
    confirmationValue: document.getElementById("confirmation-value"),
    outboundConfirmForm: document.getElementById("outbound-confirm-form"),
    outboundConfirmation: document.getElementById("outbound-confirmation"),
    confirmButton: document.getElementById("confirm-button")
  };

  function setState(element, kind, message) {
    element.className = "state " + kind;
    element.textContent = message;
    element.hidden = false;
  }

  function setSourceState(kind, message) {
    elements.sourceState.className = "source-state " + kind;
    elements.sourceState.textContent = message;
  }

  async function request(path, options) {
    const response = await fetch(path, Object.assign({
      cache: "no-store",
      headers: { Accept: "application/json" }
    }, options || {}));
    const payload = await response.json();
    if (!response.ok) {
      const error = new Error("request failed");
      error.state = payload && payload.state ? payload.state : "error";
      throw error;
    }
    return payload;
  }

  function clearChildren(element) {
    while (element.firstChild) element.removeChild(element.firstChild);
  }

  function renderSource(status) {
    if (!status.databaseAvailable) {
      setSourceState("unavailable", "Local database unavailable");
      return;
    }
    const wal = status.walPresent ? " · WAL present" : "";
    const changed = status.walChanged ? " · WAL changed" : (status.databaseChanged ? " · database changed" : "");
    setSourceState("ready", "Local database available" + wal + changed);
  }

  function renderRooms(rooms) {
    clearChildren(elements.roomsList);
    elements.roomCount.textContent = String(rooms.length);
    if (!rooms.length) {
      setState(elements.roomsState, "empty", "No rooms available.");
      return;
    }
    elements.roomsState.hidden = true;
    rooms.forEach(function (room) {
      const item = document.createElement("li");
      const button = document.createElement("button");
      const name = document.createElement("span");
      const meta = document.createElement("span");
      button.type = "button";
      button.className = "room-button" + (state.roomId === room.chatId ? " selected" : "");
      name.className = "room-name";
      name.textContent = room.displayName;
      meta.className = "room-meta";
      meta.textContent = room.roomType + " · " + String(room.activeMembersCount || 0) + " members";
      button.appendChild(name);
      button.appendChild(meta);
      button.addEventListener("click", function () {
        state.roomId = room.chatId;
        state.roomIdentity = room.displayName;
        clearOutboundDraft();
        state.searching = false;
        elements.searchKeyword.value = "";
        elements.selectedRoom.textContent = room.displayName;
        updateOutboundControls();
        renderRooms(rooms);
        loadMessages();
      });
      item.appendChild(button);
      elements.roomsList.appendChild(item);
    });
  }

  function setOutboundState(kind, message) {
    elements.outboundState.className = "source-state " + kind;
    elements.outboundState.textContent = message;
  }

  function clearOutboundDraft() {
    state.draftId = null;
    elements.outboundPreview.hidden = true;
    elements.outboundConfirmation.value = "";
  }

  function updateOutboundControls() {
    const disabled = !state.outboundUsable || state.roomId === null;
    elements.outboundText.disabled = disabled;
    elements.previewButton.disabled = disabled;
  }

  async function loadOutboundStatus() {
    try {
      const status = await request("/api/v1/outbound/status");
      state.outboundUsable = status.enabled && status.available;
      if (!status.enabled) {
        setOutboundState("unavailable", "Outbound disabled");
      } else if (status.lastStatus && outboundFailureMessages[status.lastStatus]) {
        setOutboundState("error", outboundFailureMessages[status.lastStatus]);
      } else if (!status.available) {
        setOutboundState("unavailable", "KakaoTalk helper is unavailable");
      } else {
        setOutboundState("ready", "Ready for guarded preview");
      }
    } catch (_error) {
      state.outboundUsable = false;
      setOutboundState("error", "Outbound status unavailable");
    } finally {
      updateOutboundControls();
    }
  }

  async function previewOutbound(event) {
    event.preventDefault();
    clearOutboundDraft();
    if (state.roomId === null || state.roomIdentity === null || !elements.outboundText.value) {
      setOutboundState("error", "Select a room and enter bounded text");
      return;
    }
    elements.previewButton.disabled = true;
    try {
      const preview = await request("/api/v1/outbound/preview", {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({
          roomId: state.roomId,
          targetIdentity: state.roomIdentity,
          text: elements.outboundText.value
        })
      });
      state.draftId = preview.draftId;
      elements.previewTarget.textContent = preview.targetIdentity;
      elements.previewText.textContent = preview.text;
      elements.confirmationValue.textContent = preview.confirmationValue;
      elements.outboundPreview.hidden = false;
      setOutboundState("unavailable", "Preview only — confirmation required");
    } catch (error) {
      setOutboundState("error", error.state === "mismatch" ? "Target validation failed" : "Preview refused");
    } finally {
      updateOutboundControls();
    }
  }

  async function confirmOutbound(event) {
    event.preventDefault();
    if (state.draftId === null) {
      setOutboundState("error", "Preview expired or unavailable");
      return;
    }
    elements.confirmButton.disabled = true;
    try {
      const result = await request("/api/v1/outbound/confirm", {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ draftId: state.draftId, confirmation: elements.outboundConfirmation.value })
      });
      clearOutboundDraft();
      if (result.status === "uncertain") {
        setOutboundState("unavailable", "Send invoked; delivery uncertain. Refresh messages to verify.");
      }
    } catch (error) {
      state.draftId = null;
      elements.outboundPreview.hidden = true;
      const messages = {
        disabled: "Outbound disabled",
        unavailable: "KakaoTalk helper is unavailable",
        expired: "Draft expired",
        used_or_invalid: "Draft already used or invalid",
        ...outboundFailureMessages
      };
      const validationStates = new Set(Object.keys(outboundFailureMessages));
      setOutboundState(validationStates.has(error.state) ? "error" : "unavailable", messages[error.state] || "Outbound request refused");
    } finally {
      elements.confirmButton.disabled = false;
    }
  }

  function renderMessages(messages) {
    clearChildren(elements.messagesList);
    if (!messages.length) {
      setState(elements.messagesState, "empty", state.searching ? "No messages matched that search." : "No recent messages.");
      return;
    }
    elements.messagesState.hidden = true;
    messages.forEach(function (message) {
      const item = document.createElement("li");
      const meta = document.createElement("div");
      const author = document.createElement("span");
      const time = document.createElement("time");
      const body = document.createElement("p");
      item.className = "message";
      meta.className = "message-meta";
      author.className = "message-author";
      author.textContent = message.nickName || "Unknown sender";
      time.textContent = message.time || "";
      body.className = "message-body";
      body.textContent = message.message || (message.hasAttachment ? "[attachment]" : "");
      meta.appendChild(author);
      meta.appendChild(time);
      item.appendChild(meta);
      item.appendChild(body);
      elements.messagesList.appendChild(item);
    });
  }

  async function loadStatus() {
    try {
      renderSource(await request("/api/v1/status"));
    } catch (_error) {
      setSourceState("error", "Status unavailable");
    }
  }

  async function loadRooms() {
    setState(elements.roomsState, "loading", "Loading rooms…");
    try {
      const rooms = await request("/api/v1/rooms?limit=" + String(LIMIT));
      renderRooms(Array.isArray(rooms) ? rooms : []);
    } catch (_error) {
      setState(elements.roomsState, "error", "Rooms could not be loaded.");
    }
  }

  async function loadMessages() {
    if (state.roomId === null && !state.searching) {
      setState(elements.messagesState, "empty", "Select a room to view recent messages.");
      return;
    }
    setState(elements.messagesState, "loading", "Loading messages…");
    try {
      const path = state.searching
        ? "/api/v1/search"
        : "/api/v1/rooms/" + encodeURIComponent(String(state.roomId)) + "/messages?limit=" + String(LIMIT);
      const options = state.searching ? {
        method: "POST",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({ keyword: elements.searchKeyword.value, limit: LIMIT })
      } : undefined;
      if (state.searching) elements.selectedRoom.textContent = "Search results";
      renderMessages(await request(path, options));
    } catch (_error) {
      setState(elements.messagesState, "error", "Messages could not be loaded.");
    }
  }

  async function refresh() {
    await Promise.all([loadStatus(), loadRooms(), loadOutboundStatus()]);
    await loadMessages();
  }

  function configureTimer() {
    if (state.timer !== null) window.clearInterval(state.timer);
    const seconds = Number(elements.interval.value);
    if (seconds > 0 && seconds <= 60) state.timer = window.setInterval(refresh, seconds * 1000);
  }

  elements.refresh.addEventListener("click", refresh);
  elements.interval.addEventListener("change", configureTimer);
  elements.searchForm.addEventListener("submit", function (event) {
    event.preventDefault();
    if (!elements.searchKeyword.value.trim()) {
      state.searching = false;
      loadMessages();
      return;
    }
    state.searching = true;
    loadMessages();
  });
  elements.clearSearch.addEventListener("click", function () {
    elements.searchKeyword.value = "";
    state.searching = false;
    loadMessages();
  });
  elements.outboundPreviewForm.addEventListener("submit", previewOutbound);
  elements.outboundConfirmForm.addEventListener("submit", confirmOutbound);
  refresh();
}());
