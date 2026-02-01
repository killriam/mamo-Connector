# Game Log Integration Concept

This document outlines the architecture and implementation plan for the game log reading, storage, and analysis pipeline.

## Overview

The game log integration consists of three main components:
1. **mamo-Connector** (Rust Desktop App) - File monitoring and upload
2. **new-backend** (Node.js API) - Storage and retrieval API
3. **MaMoFrontend** (React/Next.js) - Analysis and visualization

```
┌─────────────────────┐     ┌──────────────────┐     ┌─────────────────────┐
│   mamo-Connector    │     │   new-backend    │     │   MaMoFrontend      │
│   (Rust Desktop)    │────▶│   (Node.js API)  │◀────│   (React/Next.js)   │
└─────────────────────┘     └──────────────────┘     └─────────────────────┘
         │                           │                         │
    Monitors Forge                Stores in                Visualizes &
    game log dir                  PostgreSQL                Analyzes
```

---

## 1. mamo-Connector (IMPLEMENTED)

### Location
`mamo-Connector/src/gamelog.rs`

### Features Implemented
- ✅ Configurable directory path for game logs
- ✅ Background scanning toggle (enable/disable)
- ✅ File tracking (processed files stored locally)
- ✅ Game log parsing and metadata extraction
- ✅ HTTP upload to backend API
- ✅ UI tab for configuration and manual scanning

### Configuration
```rust
pub struct GameLogConfig {
    pub directory: Option<String>,
    pub background_scan_enabled: bool,
    pub scan_interval_seconds: u64,  // Default: 30
    pub api_endpoint: String,        // Default: /api/gamelog/upload
}
```

### Upload Payload
```json
{
  "filename": "game_2025-01-15_143052.txt",
  "content": "...(raw game log content)...",
  "file_size": 15234,
  "modified_timestamp": 1736951452,
  "checksum": "sha256:abc123..."
}
```

---

## 2. Backend API (TO IMPLEMENT)

### Location
`new-backend/src/routes/gamelog.ts`

### Database Schema

```sql
-- Game logs table for blob storage
CREATE TABLE game_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id),
    
    -- File metadata
    filename VARCHAR(255) NOT NULL,
    file_size INTEGER NOT NULL,
    checksum VARCHAR(128) NOT NULL,
    
    -- Content storage (blob)
    raw_content TEXT NOT NULL,
    
    -- Parsed data (optional, extracted during import)
    parsed_replay JSONB,           -- MTG Replay Notation format if parseable
    game_metadata JSONB,           -- Extracted metadata (players, date, format, etc.)
    
    -- Timestamps
    file_modified_at TIMESTAMP WITH TIME ZONE,
    uploaded_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    parsed_at TIMESTAMP WITH TIME ZONE,
    
    -- Status
    status VARCHAR(50) DEFAULT 'pending',  -- pending, parsed, parse_failed
    parse_error TEXT,
    
    -- Indexing for search
    UNIQUE(user_id, checksum)  -- Prevent duplicate uploads
);

CREATE INDEX idx_game_logs_user_id ON game_logs(user_id);
CREATE INDEX idx_game_logs_uploaded_at ON game_logs(uploaded_at DESC);
CREATE INDEX idx_game_logs_status ON game_logs(status);
```

### API Endpoints

#### POST `/api/gamelog/upload`
Upload a new game log file.

**Request:**
```typescript
interface GameLogUploadRequest {
  filename: string;
  content: string;        // Raw file content
  file_size: number;
  modified_timestamp: number;
  checksum: string;
}
```

**Response:**
```typescript
interface GameLogUploadResponse {
  success: boolean;
  id: string;             // UUID of stored log
  message: string;
  parsed?: boolean;       // Whether parsing was attempted
  duplicate?: boolean;    // If already exists
}
```

#### GET `/api/gamelog/list`
List user's uploaded game logs.

**Query params:**
- `page` (number, default: 1)
- `limit` (number, default: 20)
- `status` (string, optional): Filter by status

**Response:**
```typescript
interface GameLogListResponse {
  items: Array<{
    id: string;
    filename: string;
    file_size: number;
    uploaded_at: string;
    status: 'pending' | 'parsed' | 'parse_failed';
    game_metadata?: {
      players?: string[];
      format?: string;
      date?: string;
      winner?: string;
    };
  }>;
  total: number;
  page: number;
  limit: number;
}
```

#### GET `/api/gamelog/:id`
Get a specific game log with full content.

**Response:**
```typescript
interface GameLogDetailResponse {
  id: string;
  filename: string;
  raw_content: string;
  parsed_replay?: MTGReplayNotation;  // If successfully parsed
  game_metadata?: Record<string, any>;
  status: string;
  uploaded_at: string;
}
```

#### POST `/api/gamelog/:id/parse`
Manually trigger parsing of a game log.

**Response:**
```typescript
interface ParseResponse {
  success: boolean;
  parsed_replay?: MTGReplayNotation;
  error?: string;
}
```

#### DELETE `/api/gamelog/:id`
Delete a game log.

### Implementation Notes

1. **Authentication**: All endpoints require valid JWT token
2. **Rate Limiting**: 
   - Upload: 10 requests/minute
   - List/Get: 60 requests/minute
3. **Content Size Limit**: Max 5MB per log file
4. **Parsing**: 
   - Attempt automatic parsing on upload
   - Store parse errors for debugging
   - Allow manual re-parsing

### Forge Log Parser

Create a parser to extract structured data from Forge game logs:

```typescript
// new-backend/src/utils/forgeLogParser.ts

export interface ForgeGameMetadata {
  players: Array<{
    name: string;
    deck?: string;
    commander?: string;
  }>;
  format?: string;
  date?: Date;
  turns?: number;
  winner?: string;
}

export interface ForgeGameAction {
  turn: number;
  player: string;
  action: string;
  cards?: string[];
  timestamp?: string;
}

export function parseForgeLog(content: string): {
  metadata: ForgeGameMetadata;
  actions: ForgeGameAction[];
  canConvertToReplayNotation: boolean;
} {
  // Implementation to parse Forge-specific log format
  // Extract game events, player actions, etc.
}

export function convertToReplayNotation(
  parsedLog: ReturnType<typeof parseForgeLog>
): MTGReplayNotation | null {
  // Convert Forge log format to MTG Replay Notation
  // Returns null if conversion not possible
}
```

---

## 3. Frontend Integration (TO IMPLEMENT)

### Location
`MaMoFrontend/src/components/GameLogManager/`

### Components

#### GameLogList.tsx
Display list of uploaded game logs with filtering and pagination.

```tsx
interface GameLogListProps {
  onSelectLog: (id: string) => void;
}

// Features:
// - Table view with sortable columns
// - Status badges (pending, parsed, failed)
// - Quick actions (view, delete, re-parse)
// - Search/filter by filename, date, players
```

#### GameLogViewer.tsx
View raw log content and parsed data.

```tsx
interface GameLogViewerProps {
  logId: string;
}

// Features:
// - Raw content viewer with syntax highlighting
// - Parsed data preview
// - Export options
// - Link to analysis (if parsed to Replay Notation)
```

#### GameLogUploader.tsx
Manual upload interface (alternative to desktop app).

```tsx
// Features:
// - Drag & drop file upload
// - Multiple file support
// - Upload progress
// - Validation feedback
```

### Integration with Evaluation.tsx

Add ability to load game logs in the existing Evaluation component:

```tsx
// In Evaluation.tsx or a parent component

const [gameLogSource, setGameLogSource] = useState<'manual' | 'uploaded'>('manual');
const [selectedGameLogId, setSelectedGameLogId] = useState<string | null>(null);

// Add selector for log source
<Select value={gameLogSource} onChange={setGameLogSource}>
  <Option value="manual">Paste Replay Data</Option>
  <Option value="uploaded">From Uploaded Logs</Option>
</Select>

{gameLogSource === 'uploaded' && (
  <GameLogSelector 
    onSelect={(logId) => setSelectedGameLogId(logId)}
  />
)}
```

### API Hooks

```typescript
// MaMoFrontend/src/hooks/useGameLogs.ts

export function useGameLogs(options: { page?: number; limit?: number }) {
  return useQuery(['gameLogs', options], () => 
    api.get('/api/gamelog/list', { params: options })
  );
}

export function useGameLog(id: string | null) {
  return useQuery(['gameLog', id], () => 
    id ? api.get(`/api/gamelog/${id}`) : null,
    { enabled: !!id }
  );
}

export function useUploadGameLog() {
  return useMutation((file: File) => {
    const formData = new FormData();
    formData.append('file', file);
    return api.post('/api/gamelog/upload', formData);
  });
}

export function useParseGameLog() {
  return useMutation((id: string) => 
    api.post(`/api/gamelog/${id}/parse`)
  );
}
```

---

## 4. Data Flow

### Upload Flow
```
1. User enables background scanning in mamo-Connector
2. Forge creates game log file in configured directory
3. mamo-Connector detects new file
4. Reads file content, calculates checksum
5. POST to /api/gamelog/upload with auth token
6. Backend stores in PostgreSQL
7. Backend attempts to parse and extract metadata
8. Returns success/failure to connector
9. Connector updates local processed files list
```

### Analysis Flow
```
1. User opens Evaluation page in frontend
2. Selects "From Uploaded Logs" source
3. Frontend fetches /api/gamelog/list
4. User selects a game log
5. Frontend fetches /api/gamelog/:id
6. If parsed_replay exists:
   - Load directly into Evaluation component
7. If not parsed:
   - Show raw content
   - Offer manual parsing trigger
   - Or allow manual paste/edit
```

---

## 5. Implementation Order

### Phase 1: Backend API (Priority: HIGH)
1. Create database migration for game_logs table
2. Implement POST /api/gamelog/upload endpoint
3. Implement GET /api/gamelog/list endpoint
4. Implement GET /api/gamelog/:id endpoint
5. Add basic Forge log parser
6. Add tests for endpoints

### Phase 2: Frontend Display (Priority: MEDIUM)
1. Create GameLogList component
2. Create GameLogViewer component
3. Add hooks for API integration
4. Integrate with existing Evaluation page

### Phase 3: Advanced Features (Priority: LOW)
1. Implement Forge → Replay Notation converter
2. Add bulk upload support
3. Add game statistics dashboard
4. Add export/share functionality

---

## 6. Configuration

### Environment Variables

**Backend (.env):**
```env
# Game log settings
GAMELOG_MAX_SIZE_MB=5
GAMELOG_PARSE_ON_UPLOAD=true
GAMELOG_RETENTION_DAYS=365
```

**Frontend (.env.local):**
```env
# No additional config needed - uses existing API_URL
```

**mamo-Connector (settings.json):**
```json
{
  "gamelog_config": {
    "directory": "C:\\Users\\{user}\\AppData\\Roaming\\Forge\\gameLog",
    "background_scan_enabled": true,
    "scan_interval_seconds": 30,
    "api_endpoint": "/api/gamelog/upload"
  }
}
```

---

## 7. Security Considerations

1. **Authentication**: All uploads require valid session token
2. **Authorization**: Users can only access their own logs
3. **Content Validation**: Sanitize log content before storage
4. **Size Limits**: Enforce max file size to prevent abuse
5. **Rate Limiting**: Prevent upload spam
6. **Checksum Deduplication**: Prevent storing duplicate files

---

## 8. Future Enhancements

1. **Real-time Sync**: WebSocket notifications when new logs uploaded
2. **Deck Recognition**: Auto-detect deck from game log
3. **Statistics**: Aggregate stats across all games
4. **Sharing**: Share game logs with other users
5. **Comments**: Add notes/annotations to game logs
6. **Tags**: Categorize logs (practice, tournament, etc.)
