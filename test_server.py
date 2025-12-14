#!/usr/bin/env python3
"""
Simple test server for Mamo Connector deck API
Serves test deck data for testing the deck creation functionality
"""

import json
import http.server
import socketserver
from urllib.parse import urlparse, parse_qs
import sys

class DeckAPIHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        parsed_path = urlparse(self.path)
        
        # Handle deck API endpoint
        if parsed_path.path.startswith('/decks/'):
            deck_id = parsed_path.path.split('/decks/')[-1]
            self.handle_deck_request(deck_id)
        else:
            self.send_error(404, "Endpoint not found")
    
    def handle_deck_request(self, deck_id):
        """Handle deck data requests"""
        try:
            # Load test deck data
            with open('test_deck.json', 'r') as f:
                deck_data = json.load(f)
            
            # Customize deck name with ID for testing
            deck_data['name'] = f"Test Deck {deck_id}"
            
            # Send response
            self.send_response(200)
            self.send_header('Content-type', 'application/json')
            self.send_header('Access-Control-Allow-Origin', '*')
            self.end_headers()
            
            response = json.dumps(deck_data, indent=2)
            self.wfile.write(response.encode())
            
            print(f"✓ Served deck data for ID: {deck_id}")
            
        except FileNotFoundError:
            self.send_error(500, "Test deck data file not found")
        except Exception as e:
            print(f"Error: {e}")
            self.send_error(500, f"Server error: {str(e)}")
    
    def log_message(self, format, *args):
        """Custom logging to show requests clearly"""
        print(f"[{self.date_time_string()}] {format % args}")

def main():
    PORT = 8080
    
    print("=" * 50)
    print("Mamo Connector Test API Server")
    print("=" * 50)
    print(f"Starting server on port {PORT}")
    print(f"Deck API endpoint: http://localhost:{PORT}/decks/{{id}}")
    print("\nTo test with Mamo Connector, use:")
    print(f'mamo-connector.exe "mamoConnector://create-deck?id=12345&api_url=http://localhost:{PORT}"')
    print("\nPress Ctrl+C to stop the server")
    print("=" * 50)
    
    try:
        with socketserver.TCPServer(("", PORT), DeckAPIHandler) as httpd:
            httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n\nServer stopped.")
    except Exception as e:
        print(f"Error starting server: {e}")
        sys.exit(1)

if __name__ == "__main__":
    main()