#!/bin/bash
# Install garview desktop integration

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"

echo "Installing garview desktop integration..."

# Install desktop file
install -Dm644 "$SCRIPT_DIR/garview.desktop" "$PREFIX/share/applications/garview.desktop"
echo "  Installed desktop file to $PREFIX/share/applications/garview.desktop"

# Update desktop database
if command -v update-desktop-database &> /dev/null; then
    update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    echo "  Updated desktop database"
fi

# Offer to set as default
echo
read -p "Set garview as default for supported file types? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    # Merge mimeapps.list
    mkdir -p "$HOME/.config"

    if [ -f "$HOME/.config/mimeapps.list" ]; then
        # Backup existing
        cp "$HOME/.config/mimeapps.list" "$HOME/.config/mimeapps.list.backup"
        echo "  Backed up existing mimeapps.list"

        # Append our associations (user can manually merge if needed)
        echo "  Note: Review $SCRIPT_DIR/mimeapps.list and merge with ~/.config/mimeapps.list"
    else
        # Install fresh
        install -Dm644 "$SCRIPT_DIR/mimeapps.list" "$HOME/.config/mimeapps.list"
        echo "  Installed MIME associations"
    fi
fi

echo
echo "Installation complete!"
echo
echo "Make sure 'garview' is in your PATH:"
echo "  cargo install --path garview"
echo "  or"
echo "  cp target/release/garview ~/.local/bin/"
