import mitt, { Emitter } from "mitt";
import { toast } from "bulma-toast";

type EventType =
  | "unauthorized"
  | "session_ended"
  | "simple_message"
  | "loading_state_change"
  | "loading_progress"
  | "account_verified"
  | "similar_image"
  | "resolved_did";

const emitter: Emitter<Record<EventType, object>> = mitt();

function subscribeToEvents() {
  console.debug("Starting event subscription");

  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const endpoint = `${protocol}${window.location.host}/api/events`;

  const ws = new WebSocket(endpoint);
  let isUnauthorized = false;

  ws.onopen = () => {
    console.debug("Opened socket");
  };

  ws.onclose = () => {
    console.debug("Socket closed");
    if (isUnauthorized) {
      console.debug("Unauthorized, keeping closed");
    } else {
      setTimeout(subscribeToEvents, 1000 * 30);
    }
  };

  ws.onerror = (err) => {
    console.warn("Socket error", err);
    ws.close();
  };

  ws.onmessage = (evt) => {
    console.debug("Got event", evt);

    const payload = JSON.parse(evt.data);
    const eventType: EventType = payload["event"];

    switch (eventType) {
      case "unauthorized":
        console.debug("User is not authenticated, disconnecting");
        isUnauthorized = true;
        ws.close();

        break;

      case "session_ended":
        console.info("Session was ended");
        isUnauthorized = true;
        ws.close();

        window.location.reload();

        break;

      case "simple_message":
        if (payload["message"]) {
          toast({
            message: payload.message,
            type: "is-info",
          });
        }

        break;
    }

    emitter.emit(eventType, payload);
  };
}

subscribeToEvents();

export default emitter;
