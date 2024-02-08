import { toast } from "bulma-toast";
import emitter from "../socket";

interface SimilarImageEvent {
  media_id: string;
  link: string;
}

emitter.on("similar_image", (payload) => {
  const event = payload as SimilarImageEvent;

  if (document.querySelector(`section[data-media-id="${event.media_id}"]`)) {
    window.location.reload();
    return;
  }

  const linkElement = document.createElement("a");
  linkElement.href = event.link;
  linkElement.target = "_blank";
  linkElement.textContent = event.link;

  const text = document.createTextNode("A similar image was found: ");

  const content = document.createElement("span");
  content.appendChild(text);
  content.appendChild(linkElement);

  toast({
    message: content,
    type: "is-info",
    closeOnClick: false,
    pauseOnHover: true,
  });
});
