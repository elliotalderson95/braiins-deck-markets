#!/usr/bin/env bash
# Remove the Live Markets widget, installed with ./deploy.sh, from a Braiins Deck.
#
# Usage: ./undeploy.sh <deck-ip>
set -euo pipefail

DECK_IP="${1:?usage: ./undeploy.sh <deck-ip>}"

# The package manager needs the profile's tools on its PATH, which a plain SSH login lacks.
ssh -o BatchMode=yes -o ConnectTimeout=10 "root@$DECK_IP" \
	"PATH=/run/current-profile/bin:\$PATH /nix/var/nix/gcroots/profiles/bmc/current/bin/bmc-nix-cli remove-packages --name widget-live-markets"

echo "Removed the Live Markets widget from the Deck at $DECK_IP."
