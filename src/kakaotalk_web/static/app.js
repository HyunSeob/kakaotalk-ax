(function () {
  "use strict";

  const LIMIT = 50;
  const state = { roomId: null, timer: null, searching: false };
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
    messagesList: document.getElementById("messages-list")
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
    if (!response.ok) {
      throw new Error("request failed");
    }
    return response.json();
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
        state.searching = false;
        elements.searchKeyword.value = "";
        elements.selectedRoom.textContent = room.displayName;
        renderRooms(rooms);
        loadMessages();
      });
      item.appendChild(button);
      elements.roomsList.appendChild(item);
    });
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
    await Promise.all([loadStatus(), loadRooms()]);
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
  refresh();
}());
