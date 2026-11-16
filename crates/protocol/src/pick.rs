use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::RequestId;

/// An element someone picked in Chrome, from the extension to a core, which keeps it on the machine
/// for a prompt that names its `request`. The sender cuts the HTML short and shrinks the screenshot
/// so the message fits in one frame. Everything in it comes from the page, so it is untrusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pick {
    pub request: RequestId,
    pub url: String,
    pub title: String,
    /// A CSS selector that finds the element on the page.
    pub selector: String,
    /// The element's outer HTML.
    pub html: String,
    /// What the element looked like: a PNG, base64-encoded. None when the page couldn't be
    /// captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub screenshot_png: Option<String>,
    /// The name of the component that rendered the element, where the page's framework says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub component: Option<String>,
    /// The source file of that component, where the framework says, as the page names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub source: Option<String>,
}

/// What a chat's log keeps of a pick a prompt carried: the page's address without its query,
/// fragment or sign-in, its title and the element's selector, never its HTML or screenshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PickNote {
    pub url: String,
    pub title: String,
    pub selector: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Envelope, Message};

    #[test]
    fn a_pick_has_a_flat_tagged_shape() {
        let wire = json!({ "id": 2, "type": "pick", "request": "p-1",
            "url": "http://localhost:3000/", "title": "Home", "selector": "main > button",
            "html": "<button>Save</button>", "screenshot_png": "iVBORw0KGgo=",
            "component": "SaveButton", "source": "src/save-button.tsx" });
        let envelope = Envelope {
            id: 2,
            message: Message::Pick(Pick {
                request: "p-1".parse().unwrap(),
                url: "http://localhost:3000/".to_owned(),
                title: "Home".to_owned(),
                selector: "main > button".to_owned(),
                html: "<button>Save</button>".to_owned(),
                screenshot_png: Some("iVBORw0KGgo=".to_owned()),
                component: Some("SaveButton".to_owned()),
                source: Some("src/save-button.tsx".to_owned()),
            }),
        };

        assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), envelope);
    }

    #[test]
    fn a_pick_may_come_without_a_screenshot() {
        let wire = json!({ "id": 3, "type": "pick", "request": "p-2", "url": "http://localhost/",
            "title": "", "selector": "p", "html": "<p></p>" });
        let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();

        assert!(matches!(&envelope.message, Message::Pick(pick)
            if pick.screenshot_png.is_none() && pick.component.is_none() && pick.source.is_none()));
        assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
    }
}
