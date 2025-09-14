export interface Message {
  id: string
  username: string
  content: string
  timestamp: number
  category: string
}

export interface User {
  username: string
  websocket: WebSocket
  joinedAt: number
}

export class ChatRoom {
  private users = new Map<string, User>()
  private messages: Message[] = []
  private maxMessages = 100 // Keep last 100 messages

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url)
    
    if (url.pathname === '/websocket') {
      return this.handleWebSocket(request)
    }
    
    return new Response('Not found', { status: 404 })
  }

  private async handleWebSocket(request: Request): Promise<Response> {
    const upgradeHeader = request.headers.get('Upgrade')
    if (!upgradeHeader || upgradeHeader !== 'websocket') {
      return new Response('Expected Upgrade: websocket', { status: 426 })
    }

    const webSocketPair = new WebSocketPair()
    const [client, server] = Object.values(webSocketPair)

    server.accept()
    
    let username: string | null = null
    let userId: string | null = null

    server.addEventListener('message', async (event) => {
      try {
        const data = JSON.parse(event.data as string)
        
        switch (data.type) {
          case 'join':
            username = data.username
            userId = `${username}_${Date.now()}`
            
            // Add user to the room
            this.users.set(userId, {
              username,
              websocket: server,
              joinedAt: Date.now()
            })
            
            // Send existing messages to the new user
            server.send(JSON.stringify({
              type: 'messageHistory',
              messages: this.messages.slice(-50) // Send last 50 messages
            }))
            
            // Send current user count
            this.broadcastUserCount()
            
            // Announce user joined (optional)
            this.broadcastMessage({
              id: `system_${Date.now()}`,
              username: 'System',
              content: `${username} joined the chat`,
              timestamp: Date.now(),
              category: 'General'
            }, userId)
            
            break
            
          case 'message':
            if (!username || !userId) {
              server.send(JSON.stringify({
                type: 'error',
                message: 'Must join before sending messages'
              }))
              return
            }
            
            const message: Message = {
              id: `${userId}_${Date.now()}`,
              username,
              content: data.content,
              timestamp: Date.now(),
              category: data.category || 'General'
            }
            
            // Add to message history
            this.messages.push(message)
            
            // Keep only last maxMessages
            if (this.messages.length > this.maxMessages) {
              this.messages = this.messages.slice(-this.maxMessages)
            }
            
            // Broadcast to all users
            this.broadcastMessage(message)
            
            break
            
          default:
            server.send(JSON.stringify({
              type: 'error',
              message: 'Unknown message type'
            }))
        }
      } catch (error) {
        server.send(JSON.stringify({
          type: 'error',
          message: 'Invalid message format'
        }))
      }
    })

    server.addEventListener('close', () => {
      if (userId) {
        this.users.delete(userId)
        this.broadcastUserCount()
        
        if (username) {
          // Announce user left (optional)
          this.broadcastMessage({
            id: `system_${Date.now()}`,
            username: 'System',
            content: `${username} left the chat`,
            timestamp: Date.now(),
            category: 'General'
          })
        }
      }
    })

    server.addEventListener('error', () => {
      if (userId) {
        this.users.delete(userId)
        this.broadcastUserCount()
      }
    })

    return new Response(null, {
      status: 101,
      webSocket: client,
    })
  }

  private broadcastMessage(message: Message, excludeUserId?: string) {
    const messageData = JSON.stringify({
      type: 'message',
      ...message
    })

    for (const [userId, user] of this.users) {
      if (excludeUserId && userId === excludeUserId) continue
      
      try {
        user.websocket.send(messageData)
      } catch (error) {
        // Remove user if websocket is broken
        this.users.delete(userId)
      }
    }
  }

  private broadcastUserCount() {
    const userCountData = JSON.stringify({
      type: 'userCount',
      count: this.users.size
    })

    for (const [userId, user] of this.users) {
      try {
        user.websocket.send(userCountData)
      } catch (error) {
        // Remove user if websocket is broken
        this.users.delete(userId)
      }
    }
  }
}

export default {
  async fetch(request: Request, env: any): Promise<Response> {
    const url = new URL(request.url)
    
    // Route WebSocket connections to Durable Object
    if (url.pathname === '/chat') {
      // Get the Durable Object instance
      const id = env.CHAT_ROOM.idFromName('global-chat')
      const obj = env.CHAT_ROOM.get(id)
      
      // Forward the request with WebSocket upgrade
      const newUrl = new URL(request.url)
      newUrl.pathname = '/websocket'
      
      const newRequest = new Request(newUrl.toString(), {
        method: request.method,
        headers: request.headers,
        body: request.body,
      })
      
      return obj.fetch(newRequest)
    }
    
    return new Response('Not found', { status: 404 })
  }
}